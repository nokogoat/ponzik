//! The start screen: what Ponzik shows at launch when it does not reopen the
//! last project. A big "New project", "Open…", the templates and the house
//! demo on the left, the recent projects on the right; arrows and Enter work.
//!
//! At launch Ponzik reopens the last project (Settings › Workspace › At
//! startup), or shows this screen if that is off, the project is gone or it
//! does not load.

use std::path::PathBuf;

use ponzik_i18n::{tr, tr_fmt};
use ponzik_ui::{Canvas, Rect};
use ponzik_window::{Cursor, Key};

use super::Input;
use super::app::App;
use super::filedialog::Kind;
use super::theme::*;
use super::widgets::{Icon, draw_icon, wid};

/// What Ponzik does at launch, without a file to open.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Startup {
    /// The last project opened or saved, if it is still there.
    #[default]
    Reopen,
    /// Always the start screen.
    StartScreen,
}

impl Startup {
    pub fn id(self) -> &'static str {
        match self {
            Startup::Reopen => "reopen",
            Startup::StartScreen => "start_screen",
        }
    }

    pub fn from_id(id: &str) -> Startup {
        if id == "start_screen" { Startup::StartScreen } else { Startup::Reopen }
    }
}

/// The start screen while it is shown.
#[derive(Clone, Debug, Default)]
pub struct StartScreen {
    /// Recent projects still on disk, newest first.
    pub recent: Vec<PathBuf>,
    /// Keyboard selection, in [`StartScreen::items`] order.
    pub sel: usize,
}

/// One thing the start screen offers.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    New,
    Open,
    Recent(PathBuf),
    Template(usize),
    Demo,
}

impl StartScreen {
    pub fn new(recent: Vec<PathBuf>) -> Self {
        // The newest project is one Enter away; without any, a new project is.
        let sel = if recent.is_empty() { 0 } else { 2 };
        StartScreen { recent, sel }
    }

    /// Everything, in keyboard order: new, open, the recent projects, the
    /// templates (but the empty one) and the demo.
    pub fn items(&self) -> Vec<Item> {
        let mut v = vec![Item::New, Item::Open];
        v.extend(self.recent.iter().cloned().map(Item::Recent));
        v.extend((1..ponzik_project::templates().len()).map(Item::Template));
        v.push(Item::Demo);
        v
    }

    /// Arrows move the selection; Enter picks it; Ctrl+N and Ctrl+O are
    /// there too; Escape leaves an empty project.
    pub fn keys(&mut self, keys: &[(Key, ponzik_window::Modifiers)]) -> Option<Option<Item>> {
        let items = self.items();
        for &(key, mods) in keys {
            match key {
                Key::Down | Key::Right => self.sel = (self.sel + 1).min(items.len() - 1),
                Key::Up | Key::Left => self.sel = self.sel.saturating_sub(1),
                Key::Home => self.sel = 0,
                Key::End => self.sel = items.len() - 1,
                Key::Enter | Key::Space => return Some(items.get(self.sel).cloned()),
                Key::Char('n') if mods.ctrl => return Some(Some(Item::New)),
                Key::Char('o') if mods.ctrl => return Some(Some(Item::Open)),
                Key::Escape => return Some(None),
                _ => {}
            }
        }
        None
    }
}

impl App {
    /// Launch without a file: the last project, or the start screen.
    pub fn startup(&mut self) {
        if self.prefs.startup == Startup::Reopen
            && let Some(last) = self.recent_files.first().filter(|p| p.is_file()).cloned()
            && self.open_file(last)
        {
            return;
        }
        self.show_start_screen();
    }

    pub fn show_start_screen(&mut self) {
        self.recent_files.retain(|p| p.exists());
        self.start = Some(StartScreen::new(self.recent_files.clone()));
        self.anim.set(wid("start", 0, 0), 0.0);
    }

    fn start_pick(&mut self, item: Option<Item>) {
        match item {
            None => self.start = None,
            Some(Item::New) => self.new_from_template(0),
            Some(Item::Template(i)) => self.new_from_template(i),
            Some(Item::Demo) => self.load_demo(0),
            Some(Item::Recent(path)) => {
                // A project that no longer loads stays listed: the toast says why.
                self.open_file(path);
            }
            // The start screen stays under the dialog: cancelling comes back here.
            Some(Item::Open) => self.open_open_dialog(),
        }
    }

    /// Draws the start screen over the (empty) project.
    pub(super) fn start_layer(&mut self, c: &mut Canvas, input: &Input, screen: Rect) {
        let Some(mut st) = self.start.take() else { return };
        let t = self.theme;
        let mut picked: Option<Option<Item>> = None;
        if self.popup.is_none() {
            picked = st.keys(&input.keys);
        }
        let appear = self.anim.to(wid("start", 0, 0), 1.0, T_LAYOUT);
        c.fill_rect(screen, t.bg);
        let rise = ((1.0 - appear) * 12.0).round();
        let cw = 1120.0f32.min(screen.w - 96.0);
        let x0 = ((screen.w - cw) * 0.5).round();
        // Centred in the height when there is room: header, the left column
        // (new, open, three rows of templates) or the recent projects.
        let left_h: f32 = 128.0 + 12.0 + 72.0 + 44.0 + 3.0 * 64.0 + 2.0 * 10.0;
        let right_h = 22.0 + (st.recent.len().max(3) as f32) * 70.0;
        let content_h = 92.0 + left_h.max(right_h);
        let y0 = ((screen.h - 72.0 - content_h) * 0.42).clamp(28.0, 120.0) + rise;

        // Header.
        let mark = Rect::new(x0, y0, 56.0, 56.0);
        ponzik_ui::brand::draw_mark(c, mark);
        self.text(c, self.f.sans_med, 30.0, mark.right() + 18.0, y0 + 20.0, "Ponzik", t.text);
        self.text(c, self.f.sans, 14.0, mark.right() + 19.0, y0 + 46.0, tr("Start something new, or pick up where you left off"), t.text2);
        let v = crate::build_info::version().semver();
        self.text_right(c, self.f.mono, 11.5, x0 + cw, y0 + 20.0, &v, t.muted);

        let items = st.items();
        let sel = items.get(st.sel).cloned();
        let top = y0 + 92.0;
        let foot_y = screen.h - 56.0;
        let lw = (cw * 0.42).round();
        let left = Rect::new(x0, top, lw, foot_y - top - 16.0);
        let right = Rect::new(x0 + lw + 36.0, top, cw - lw - 36.0, foot_y - top - 16.0);
        let free = self.popup.is_none();
        let over = |r: Rect| free && input.inside && r.contains(input.x, input.y);

        // New project: the big one.
        let new_r = Rect::new(left.x, left.y, left.w, 128.0);
        let id = wid("start-new", 0, 0);
        let hov = over(new_r);
        let hv = self.hover_anim(id, hov || sel == Some(Item::New));
        let press = self.anim.to(id ^ 0x5052, if hov && input.down { 0.985 } else { 1.0 }, T_PRESS);
        let b = new_r.scale(press);
        c.fill_rrect(b.translate(0.0, 6.0).inset(-2.0), 20.0, t.shade(60.0 * hv));
        c.fill_rrect(b, 18.0, t.accent.mix(t.highlight, 0.1 * hv));
        let (cx, cy) = (b.x + 52.0, b.y + b.h * 0.5);
        c.fill_circle(cx, cy, 26.0, t.on_accent.alpha(0.14));
        draw_icon(c, Icon::Plus, cx, cy, 26.0, t.on_accent);
        self.text(c, self.f.sans_med, 21.0, b.x + 96.0, cy - 12.0, tr("New project"), t.on_accent);
        self.text_fit(c, self.f.sans, 13.0, b.x + 96.0, cy + 14.0, tr("An empty project, ready for your first sound"), b.w - 200.0, t.on_accent.alpha(0.8));
        self.start_keycap(c, b.right() - 18.0, b.y + 24.0, "Ctrl+N", t.on_accent);
        if hov {
            self.set_cursor(Cursor::Hand);
            if input.pressed {
                picked = Some(Some(Item::New));
            }
        }

        // Open.
        let open_r = Rect::new(left.x, new_r.bottom() + 12.0, left.w, 72.0);
        if self.start_card(c, input, wid("start-open", 0, 0), open_r, sel == Some(Item::Open), Icon::Folder, tr("Open a project…"), tr("Ponzik, FL Studio, Ableton Live, REAPER, LMMS, Ardour, MusicXML or MIDI"), "Ctrl+O") {
            picked = Some(Some(Item::Open));
        }

        // Templates and the demo.
        let grid_y = open_r.bottom() + 28.0;
        self.text(c, self.f.sans_med, 11.5, left.x + 2.0, grid_y, tr("Templates"), t.muted);
        let templates = ponzik_project::templates();
        let gap = 10.0;
        let tw = ((left.w - gap) / 2.0).floor();
        let th = 64.0;
        let mut cards: Vec<(Item, Icon, String, String)> = templates
            .iter()
            .enumerate()
            .skip(1)
            .map(|(i, (name, make))| (Item::Template(i), super::app::template_icon(i), tr(name).to_string(), tr_fmt!("{} BPM", format!("{:.0}", make().bpm))))
            .collect();
        cards.push((Item::Demo, Icon::Play, tr("House demo").to_string(), tr("Listen first").to_string()));
        for (k, (item, icon, name, detail)) in cards.into_iter().enumerate() {
            let r = Rect::new(left.x + (k % 2) as f32 * (tw + gap), grid_y + 16.0 + (k / 2) as f32 * (th + gap), tw, th);
            if r.bottom() > left.bottom() {
                break;
            }
            let id = wid("start-tpl", k, 0);
            let hov = over(r);
            let on = sel.as_ref() == Some(&item);
            let hv = self.hover_anim(id, hov || on);
            c.fill_rrect(r, 14.0, t.raised.mix(t.raised_hi, hv * 0.7));
            c.stroke_rrect(r, 14.0, 1.0, t.hairline.mix(t.accent, if on { 0.8 } else { 0.0 }));
            let tile = Rect::new(r.x + 12.0, r.y + 16.0, 32.0, 32.0);
            c.fill_rrect(tile, 9.0, t.accent.alpha(0.12 + 0.08 * hv));
            draw_icon(c, icon, tile.x + 16.0, tile.y + 16.0, 16.0, t.accent);
            self.text_fit(c, self.f.sans_med, 12.5, tile.right() + 10.0, r.y + 25.0, &name, r.right() - tile.right() - 18.0, t.text2.mix(t.text, hv));
            self.text_fit(c, self.f.mono, 10.5, tile.right() + 10.0, r.y + 43.0, &detail, r.right() - tile.right() - 18.0, t.muted);
            if hov {
                self.set_cursor(Cursor::Hand);
                if input.pressed {
                    picked = Some(Some(item));
                }
            }
        }

        // Recent projects.
        let count = st.recent.len();
        self.text(c, self.f.sans_med, 11.5, right.x + 2.0, right.y + 8.0, tr("Recent projects"), t.muted);
        if count > 0 {
            self.text_right(c, self.f.mono, 11.0, right.right(), right.y + 8.0, &count.to_string(), t.muted);
        }
        let list = Rect::new(right.x, right.y + 22.0, right.w, right.h - 22.0);
        if st.recent.is_empty() {
            let card = Rect::new(list.x, list.y, list.w, 180.0);
            c.fill_rrect(card, 16.0, t.panel);
            c.stroke_rrect(card, 16.0, 1.0, t.hairline);
            let (cx, cy) = (card.x + card.w * 0.5, card.y + 70.0);
            c.fill_circle(cx, cy, 26.0, t.raised.alpha(0.8));
            draw_icon(c, Icon::History, cx, cy, 22.0, t.muted);
            self.text_center(c, self.f.sans_med, 14.0, cx, cy + 48.0, tr("No recent projects yet"), t.text2);
            self.text_center(c, self.f.sans, 12.0, cx, cy + 70.0, tr("Projects you open or save will show here"), t.muted);
        }
        let row_h = 64.0;
        let prev = c.push_clip(list);
        for (k, path) in st.recent.iter().enumerate() {
            let r = Rect::new(list.x, list.y + k as f32 * (row_h + 6.0), list.w, row_h);
            if r.y > list.bottom() {
                break;
            }
            let item = Item::Recent(path.clone());
            let id = wid("start-recent", k, 0);
            let hov = over(r) && list.contains(input.x, input.y);
            let on = sel.as_ref() == Some(&item);
            let hv = self.hover_anim(id, hov);
            let onv = self.anim.to(id ^ 0x4f4e, on as u8 as f32, T_HOVER);
            c.fill_rrect(r, 14.0, t.panel.mix(t.raised, hv.max(onv)));
            if onv > 0.01 {
                c.stroke_rrect(r, 14.0, 1.0, t.accent.alpha(0.7 * onv));
                c.fill_rrect(Rect::new(r.x + 4.0, r.y + 18.0, 3.0, r.h - 36.0), 1.5, t.accent.alpha(onv));
            }
            let kind = Kind::of(path, false);
            let cy = r.y + r.h * 0.5;
            self.start_badge(c, kind, r.x + 34.0, cy);
            let name = path.file_stem().map_or(String::new(), |s| s.to_string_lossy().into_owned());
            let text_w = r.w - 70.0 - 130.0;
            self.text_fit(c, self.f.sans_med, 15.0, r.x + 66.0, cy - 10.0, &name, text_w, t.text);
            let dir = path.parent().map_or(String::new(), super::compat::short_path);
            self.text_fit(c, self.f.sans, 12.0, r.x + 66.0, cy + 11.0, &dir, text_w, t.text2);
            let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
            self.text_right(c, self.f.sans, 12.0, r.right() - 18.0, cy - 9.0, &super::filedialog::age_label(modified), t.text2);
            self.text_right(c, self.f.sans, 11.5, r.right() - 18.0, cy + 11.0, kind.label(), kind.color(&t).alpha(0.9));
            if hov {
                self.set_cursor(Cursor::Hand);
                self.set_tip(&path.display().to_string(), "");
                if input.pressed {
                    picked = Some(Some(item));
                }
            }
        }
        c.set_clip(Some(prev));

        // Footer: the startup choice, the keys.
        let fy = foot_y + 18.0;
        c.fill_rect(Rect::new(x0, foot_y - 6.0, cw, 1.0), t.hairline);
        let reopen = self.prefs.startup == Startup::Reopen;
        let label = tr("Reopen the last project at startup");
        let lw2 = self.fonts.measure(self.f.sans, 12.5, label);
        let row = Rect::new(x0, fy - 14.0, 30.0 + lw2, 28.0);
        let id = wid("start-reopen", 0, 0);
        let hov = over(row);
        let hv = self.hover_anim(id, hov);
        let on = self.anim.to(id ^ 0x4f4e, reopen as u8 as f32, T_HOVER);
        let bx = Rect::new(row.x, fy - 9.0, 18.0, 18.0);
        c.fill_rrect(bx, 5.0, t.well.mix(t.accent, on));
        c.stroke_rrect(bx, 5.0, 1.0, t.hairline.mix(t.muted, hv).mix(t.accent, on));
        if on > 0.05 {
            draw_icon(c, Icon::Check, bx.x + 9.0, bx.y + 9.0, 12.0, t.on_accent.alpha(on));
        }
        self.text(c, self.f.sans, 12.5, bx.right() + 10.0, fy, label, t.text2.mix(t.text, hv));
        if hov {
            self.set_cursor(Cursor::Hand);
            self.set_tip(tr("Otherwise this screen opens at every launch · also in Settings › Workspace"), "");
            if input.pressed {
                self.prefs.startup = if reopen { Startup::StartScreen } else { Startup::Reopen };
                self.save_settings();
            }
        }
        let keys = tr("Arrows to choose · Enter to open · Esc for an empty project");
        self.text_right(c, self.f.sans, 12.0, x0 + cw, fy, keys, t.muted);

        if appear < 1.0 {
            c.fill_rect(screen, t.bg.alpha((1.0 - appear) * 0.6));
        }
        self.start = Some(st);
        if let Some(item) = picked {
            self.start_pick(item);
        }
    }

    fn start_keycap(&mut self, c: &mut Canvas, right: f32, cy: f32, key: &str, ink: ponzik_ui::Color) {
        let kw = self.fonts.measure(self.f.mono, 11.0, key) + 12.0;
        let cap = Rect::new(right - kw, cy - 10.0, kw, 20.0);
        c.stroke_rrect(cap, 5.0, 1.0, ink.alpha(0.45));
        self.text_center(c, self.f.mono, 11.0, cap.x + kw * 0.5, cy, key, ink.alpha(0.85));
    }

    /// A wide card: an icon tile, a title and a line, a key on the right.
    #[allow(clippy::too_many_arguments)]
    fn start_card(&mut self, c: &mut Canvas, input: &Input, id: u64, r: Rect, on: bool, icon: Icon, title: &str, sub: &str, key: &str) -> bool {
        let t = self.theme;
        let hov = input.inside && r.contains(input.x, input.y) && self.popup.is_none();
        let hv = self.hover_anim(id, hov || on);
        c.fill_rrect(r, 16.0, t.raised.mix(t.raised_hi, hv * 0.7));
        c.stroke_rrect(r, 16.0, 1.0, t.hairline.mix(t.accent, if on { 0.8 } else { 0.0 }));
        let tile = Rect::new(r.x + 16.0, r.y + (r.h - 40.0) * 0.5, 40.0, 40.0);
        c.fill_rrect(tile, 11.0, t.accent.alpha(0.14 + 0.08 * hv));
        draw_icon(c, icon, tile.x + 20.0, tile.y + 20.0, 19.0, t.accent);
        let cy = r.y + r.h * 0.5;
        self.text(c, self.f.sans_med, 15.0, tile.right() + 16.0, cy - 9.0, title, t.text);
        self.text_fit(c, self.f.sans, 12.0, tile.right() + 16.0, cy + 11.0, sub, r.right() - tile.right() - 110.0, t.text2);
        self.start_keycap(c, r.right() - 18.0, cy, key, t.text2);
        if hov {
            self.set_cursor(Cursor::Hand);
        }
        hov && input.pressed
    }

    fn start_badge(&mut self, c: &mut Canvas, kind: Kind, cx: f32, cy: f32) {
        let t = self.theme;
        let tile = Rect::new(cx - 20.0, cy - 20.0, 40.0, 40.0);
        c.fill_rrect(tile, 11.0, kind.color(&t).alpha(0.16));
        if kind == Kind::Ponzik {
            ponzik_ui::brand::draw_mark(c, tile.inset(9.0));
        } else {
            draw_icon(c, kind.icon(), cx, cy, 19.0, kind.color(&t));
        }
    }

    /// Captures: `PONZIK_START=1` (no recent project) or `PONZIK_START=<folder>`
    /// (its project files as the recent ones), `,sel:n` for the selection.
    pub fn screenshot_start(&mut self, spec: &str) {
        let mut parts = spec.split(',').map(str::trim);
        let head = parts.next().unwrap_or("1");
        let recent: Vec<PathBuf> = if head == "1" || head.is_empty() {
            Vec::new()
        } else {
            let mut found: Vec<super::filedialog::Entry> = super::filedialog::list_dir(std::path::Path::new(head))
                .unwrap_or_default()
                .into_iter()
                .filter(|e| !matches!(e.kind, Kind::Folder | Kind::Midi | Kind::Audio | Kind::Other))
                .collect();
            found.sort_by_key(|e| std::cmp::Reverse(e.modified));
            found.into_iter().map(|e| e.path).collect()
        };
        self.replace_project(ponzik_project::Project::empty(), None);
        self.recent_files = recent;
        self.show_start_screen();
        for p in parts {
            if let (Some(n), Some(st)) = (p.strip_prefix("sel:").and_then(|n| n.parse().ok()), self.start.as_mut()) {
                st.sel = n;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyboard_order_and_keys() {
        let none = ponzik_window::Modifiers::default();
        let ctrl = ponzik_window::Modifiers { ctrl: true, ..Default::default() };
        let mut st = StartScreen::new(Vec::new());
        assert_eq!(st.items()[st.sel], Item::New, "nothing recent: a new project is one Enter away");
        assert_eq!(st.keys(&[(Key::Enter, none)]), Some(Some(Item::New)));
        let a = PathBuf::from("/m/a.ponzik");
        let mut st = StartScreen::new(vec![a.clone(), PathBuf::from("/m/b.flp")]);
        assert_eq!(st.items()[st.sel], Item::Recent(a.clone()));
        st.keys(&[(Key::Down, none)]);
        assert_eq!(st.items()[st.sel], Item::Recent(PathBuf::from("/m/b.flp")));
        st.keys(&[(Key::Up, none), (Key::Up, none)]);
        assert_eq!(st.keys(&[(Key::Enter, none)]), Some(Some(Item::Open)));
        st.keys(&[(Key::End, none)]);
        assert_eq!(st.items()[st.sel], Item::Demo);
        assert_eq!(st.keys(&[(Key::Char('o'), ctrl)]), Some(Some(Item::Open)));
        assert_eq!(st.keys(&[(Key::Escape, none)]), Some(None));
        assert_eq!(Startup::from_id(Startup::StartScreen.id()), Startup::StartScreen);
        assert_eq!(Startup::from_id("anything"), Startup::Reopen);
    }

    fn test_app() -> App {
        let project = ponzik_project::Project::empty();
        let (_engine, handle) = project.build_engine(48_000.0, &|_| None);
        App::new(handle, project, 48_000)
    }

    #[test]
    fn launch_reopens_the_last_project_or_shows_the_screen() {
        let d = std::env::temp_dir().join(format!("ponzik-start-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let path = d.join("last.ponzik");
        let mut song = ponzik_project::demo_project();
        song.name = "Last one".into();
        ponzik_project::save_to_file(&song, &path).unwrap();

        // Reopen: the last project comes back, no start screen.
        let mut app = test_app();
        app.recent_files = vec![path.clone()];
        app.startup();
        assert_eq!(app.project.name, "Last one");
        assert_eq!(app.project_path.as_deref(), Some(path.as_path()));
        assert!(app.start.is_none());

        // Gone or broken: the start screen.
        let mut app = test_app();
        app.recent_files = vec![d.join("gone.ponzik")];
        app.startup();
        assert!(app.start.as_ref().is_some_and(|s| s.recent.is_empty()));
        std::fs::write(d.join("broken.ponzik"), "{ not json").unwrap();
        let mut app = test_app();
        app.recent_files = vec![d.join("broken.ponzik"), path.clone()];
        app.startup();
        assert!(app.start.is_some());

        // The start screen when asked for, and a pick closes it.
        let mut app = test_app();
        app.prefs.startup = Startup::StartScreen;
        app.recent_files = vec![path.clone()];
        app.startup();
        assert!(app.start.is_some() && app.project_path.is_none());
        app.start_pick(Some(Item::Recent(path.clone())));
        assert!(app.start.is_none());
        assert_eq!(app.project.name, "Last one");
        // New project and Escape.
        app.show_start_screen();
        app.start_pick(Some(Item::New));
        assert!(app.start.is_none() && app.project.channels.is_empty());
        app.show_start_screen();
        app.start_pick(None);
        assert!(app.start.is_none());
        let _ = std::fs::remove_dir_all(&d);
    }
}
