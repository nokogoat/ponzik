//! Themes on disk: the built-ins, the bundled examples and the user's files in
//! `<config dir>/themes/` (`.ponzik-theme` or `.css`), reloaded live when they
//! change so a theme can be written in any text editor.

use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

use ponzik_i18n::{tr, tr_fmt};
use ponzik_ui::Canvas;

use super::app::App;
use super::compat;
use super::theme::{BUILTINS, TOKENS, Theme, Val, set_pattern_palette};
use super::themecss::{self, Area, Base, CssError};

/// Example themes shipped with Ponzik, read-only like the built-ins.
const EXAMPLES: [(&str, &str); 6] = [
    ("midnight", include_str!("../../../../assets/themes/midnight.ponzik-theme")),
    ("paper-light", include_str!("../../../../assets/themes/paper-light.ponzik-theme")),
    ("synthwave", include_str!("../../../../assets/themes/synthwave.ponzik-theme")),
    ("high-contrast", include_str!("../../../../assets/themes/high-contrast.ponzik-theme")),
    ("carrot-studio", include_str!("../../../../assets/themes/carrot-studio.ponzik-theme")),
    ("unable-live", include_str!("../../../../assets/themes/unable-live.ponzik-theme")),
];

pub const EXTENSIONS: [&str; 2] = ["ponzik-theme", "css"];

#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    Builtin(usize),
    Example,
    /// A file the user can edit.
    User(PathBuf),
}

/// A theme with its areas.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ThemeSet {
    pub root: Theme,
    pub areas: [Option<Theme>; 7],
}

impl ThemeSet {
    pub fn area(&self, a: Area) -> Theme {
        self.areas[a.index()].unwrap_or(self.root)
    }
}

#[derive(Clone, Debug)]
pub struct ThemeEntry {
    /// Saved in the settings: built-in id, example slug, or `user:<file name>`.
    pub id: String,
    pub name: String,
    pub source: Source,
    pub text: String,
    pub mtime: Option<SystemTime>,
    /// Id of the theme it inherits from (itself for a built-in).
    pub base_id: String,
    /// What this theme's own children inherit.
    pub inherit: Base,
    pub set: ThemeSet,
    pub errors: Vec<CssError>,
}

impl ThemeEntry {
    pub fn editable(&self) -> bool {
        matches!(self.source, Source::User(_))
    }

    pub fn path(&self) -> Option<&Path> {
        match &self.source {
            Source::User(p) => Some(p),
            _ => None,
        }
    }

    /// Raw text of `--name` in `:root`, if the file sets it.
    pub fn declared(&self, token: &str) -> Option<String> {
        let sheet = themecss::parse(&self.text);
        sheet.blocks.iter().filter(|b| b.scopes == [themecss::Scope::Root]).flat_map(|b| b.decls.iter()).rev().find(|d| d.name == token).map(|d| d.value.clone())
    }
}

/// Previous values of one token of one theme (theme id, token index): `None`
/// when the token was inherited.
type UndoStack = ((String, usize), Vec<Option<String>>);

/// Editor state of the Appearance tab.
#[derive(Default)]
pub struct EditorState {
    pub group: usize,
    pub token: Option<usize>,
    pub undo: Vec<UndoStack>,
    /// Hex field being typed.
    pub hex: Option<String>,
    /// HSV being dragged in the picker (keeps the hue when saturation is 0).
    pub hsv: Option<(f32, f32, f32)>,
    pub delete_armed: Option<String>,
    pub scroll: f32,
    pub list_scroll: f32,
    /// A write is waiting for the end of a drag.
    pub dirty: bool,
}

pub struct ThemeStore {
    pub entries: Vec<ThemeEntry>,
    pub active: usize,
    pub dir: PathBuf,
    pub set: ThemeSet,
    last_poll: Option<Instant>,
    /// User themes already read from `dir`.
    pub loaded: bool,
    pub editor: EditorState,
}

fn slug(s: &str) -> String {
    let mut out = String::new();
    for ch in s.trim().chars() {
        if ch.is_alphanumeric() {
            out.extend(ch.to_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

pub fn themes_dir() -> PathBuf {
    compat::config_dir().join("themes")
}

fn is_theme_file(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| EXTENSIONS.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

impl Default for ThemeStore {
    fn default() -> Self {
        Self::new(themes_dir())
    }
}

impl ThemeStore {
    pub fn new(dir: PathBuf) -> Self {
        let mut entries = Vec::new();
        for (i, b) in BUILTINS.iter().enumerate() {
            let base = Base { raw: b.raw, overrides: Vec::new() };
            let root = base.theme();
            entries.push(ThemeEntry {
                id: b.id.to_string(),
                name: b.label.to_string(),
                source: Source::Builtin(i),
                text: String::new(),
                mtime: None,
                base_id: b.id.to_string(),
                inherit: base,
                set: ThemeSet { root, areas: [None; 7] },
                errors: Vec::new(),
            });
        }
        let mut store = Self { entries, active: 0, dir, set: ThemeSet { root: super::theme::ponzik(), areas: [None; 7] }, last_poll: None, loaded: false, editor: EditorState::default() };
        for (id, text) in EXAMPLES {
            let e = store.build(id.to_string(), Source::Example, text.to_string(), None);
            store.entries.push(e);
        }
        store
    }

    pub fn active(&self) -> &ThemeEntry {
        &self.entries[self.active]
    }

    pub fn active_id(&self) -> &str {
        &self.entries[self.active].id
    }

    /// Finds a theme by id, name or slug (case-insensitive).
    pub fn find(&self, key: &str) -> Option<usize> {
        let k = key.trim();
        let s = slug(k);
        self.entries.iter().position(|e| e.id == k).or_else(|| {
            self.entries.iter().position(|e| {
                e.id.eq_ignore_ascii_case(k)
                    || e.name.eq_ignore_ascii_case(k)
                    || slug(&e.name) == s
                    || e.id.strip_prefix("user:").is_some_and(|f| slug(f.rsplit_once('.').map_or(f, |(a, _)| a)) == s)
                    || matches!(e.source, Source::Builtin(i) if slug(tr(BUILTINS[i].label)) == s)
            })
        })
    }

    /// Bases a file may name: built-ins and examples.
    fn base_of(&self, name: &str) -> Option<&ThemeEntry> {
        self.find(name).map(|i| &self.entries[i]).filter(|e| !e.editable())
    }

    /// Reads a theme file's text into an entry (never fails: errors are listed).
    pub fn build(&self, id: String, source: Source, text: String, mtime: Option<SystemTime>) -> ThemeEntry {
        let sheet = themecss::parse(&text);
        let mut errors = Vec::new();
        let base_name = themecss::base_name(&sheet).unwrap_or_else(|| "ponzik".into());
        let base_entry = match self.base_of(&base_name) {
            Some(e) => e,
            None => {
                let line = sheet.blocks.iter().flat_map(|b| b.decls.iter()).find(|d| d.name == "base").map_or(1, |d| d.line);
                errors.push(CssError { line, msg: tr_fmt!("unknown base theme \"{}\", using Ponzik", base_name.as_str()) });
                &self.entries[0]
            }
        };
        let parent = base_entry.inherit.clone();
        let base_id = base_entry.id.clone();
        let compiled = themecss::compile(&sheet, &parent);
        errors.extend(compiled.errors.iter().cloned());
        errors.sort_by_key(|e| e.line);
        let mut root_ov = parent.overrides.clone();
        root_ov.extend(compiled.root.iter().copied());
        let root = Theme::resolve(&parent.raw, &root_ov);
        let mut areas = [None; 7];
        for (a, list) in &compiled.areas {
            if list.is_empty() {
                continue;
            }
            let mut ov = root_ov.clone();
            ov.extend(list.iter().copied());
            areas[a.index()] = Some(Theme::resolve(&parent.raw, &ov));
        }
        let fallback = match &source {
            Source::User(p) => p.file_stem().map_or_else(|| id.clone(), |s| s.to_string_lossy().into_owned()),
            _ => id.clone(),
        };
        ThemeEntry {
            id,
            name: compiled.name.filter(|n| !n.trim().is_empty()).unwrap_or(fallback),
            source,
            text,
            mtime,
            base_id,
            inherit: Base { raw: parent.raw, overrides: root_ov },
            set: ThemeSet { root, areas },
            errors,
        }
    }

    fn read_file(&self, path: &Path) -> Option<ThemeEntry> {
        let text = std::fs::read(path).ok()?;
        let text = String::from_utf8_lossy(&text).into_owned();
        let mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        let file = path.file_name()?.to_string_lossy().into_owned();
        let id = if path.parent() == Some(self.dir.as_path()) { format!("user:{file}") } else { format!("file:{}", path.display()) };
        Some(self.build(id, Source::User(path.to_path_buf()), text, mtime))
    }

    /// User themes, sorted by name, after the built-ins and the examples.
    pub fn load_user(&mut self) {
        self.loaded = true;
        let mut files: Vec<PathBuf> = std::fs::read_dir(&self.dir)
            .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.is_file() && is_theme_file(p)).collect())
            .unwrap_or_default();
        files.sort();
        let active = self.active_id().to_string();
        self.entries.retain(|e| !matches!(&e.source, Source::User(p) if p.parent() == Some(self.dir.as_path())));
        for f in files {
            if let Some(e) = self.read_file(&f) {
                self.entries.push(e);
            }
        }
        self.active = self.find(&active).unwrap_or(0);
        self.refresh();
    }

    /// Adds (or replaces) a theme file from anywhere, e.g. `PONZIK_THEME=<path>`.
    pub fn add_file(&mut self, path: &Path) -> Option<usize> {
        let e = self.read_file(path)?;
        let i = match self.entries.iter().position(|x| x.id == e.id) {
            Some(i) => {
                self.entries[i] = e;
                i
            }
            None => {
                self.entries.push(e);
                self.entries.len() - 1
            }
        };
        Some(i)
    }

    /// The active theme's set, after a change.
    pub fn refresh(&mut self) {
        self.active = self.active.min(self.entries.len() - 1);
        self.set = self.entries[self.active].set;
    }

    pub fn select(&mut self, i: usize) {
        self.active = i.min(self.entries.len() - 1);
        self.editor.token = None;
        self.editor.hex = None;
        self.editor.hsv = None;
        self.refresh();
    }

    /// Hot reload: user files changed, appeared or disappeared since the last
    /// look (at most twice a second). Returns the themes that now have errors.
    pub fn poll(&mut self, now: Instant) -> Option<Vec<(String, Vec<CssError>)>> {
        if self.last_poll.is_some_and(|t| now.duration_since(t).as_secs_f32() < 0.5) {
            return None;
        }
        self.last_poll = Some(now);
        let mut changed = false;
        let mut reports = Vec::new();
        // Files edited or removed.
        let mut i = 0;
        while i < self.entries.len() {
            let Source::User(path) = self.entries[i].source.clone() else {
                i += 1;
                continue;
            };
            match std::fs::metadata(&path).and_then(|m| m.modified()) {
                Ok(m) if Some(m) != self.entries[i].mtime => {
                    if let Some(e) = self.read_file(&path) {
                        if !e.errors.is_empty() {
                            reports.push((e.name.clone(), e.errors.clone()));
                        }
                        let keep_id = self.entries[i].id.clone();
                        self.entries[i] = ThemeEntry { id: keep_id, ..e };
                        changed = true;
                    }
                    i += 1;
                }
                Ok(_) => i += 1,
                Err(_) => {
                    let was_active = i == self.active;
                    self.entries.remove(i);
                    if was_active {
                        self.active = 0;
                    } else if i < self.active {
                        self.active -= 1;
                    }
                    changed = true;
                }
            }
        }
        // New files.
        if self.loaded
            && let Ok(rd) = std::fs::read_dir(&self.dir)
        {
            for p in rd.flatten().map(|e| e.path()).filter(|p| p.is_file() && is_theme_file(p)) {
                if !self.entries.iter().any(|e| e.path() == Some(p.as_path()))
                    && let Some(e) = self.read_file(&p)
                {
                    self.entries.push(e);
                    changed = true;
                }
            }
        }
        if changed {
            let before = self.set;
            self.refresh();
            if self.set == before && reports.is_empty() {
                return Some(Vec::new());
            }
        }
        changed.then_some(reports)
    }

    /// A free file name in the themes folder.
    fn free_path(&self, name: &str) -> PathBuf {
        let stem = compat::file_name(name);
        let mut path = self.dir.join(format!("{stem}.ponzik-theme"));
        let mut n = 2;
        while path.exists() {
            path = self.dir.join(format!("{stem} {n}.ponzik-theme"));
            n += 1;
        }
        path
    }

    fn write(&mut self, i: usize) -> std::io::Result<()> {
        let Some(path) = self.entries[i].path().map(Path::to_path_buf) else { return Ok(()) };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, &self.entries[i].text)?;
        std::fs::rename(&tmp, &path)?;
        self.entries[i].mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        Ok(())
    }

    /// New user theme copied from `i`, selected. Returns its index.
    pub fn duplicate(&mut self, i: usize) -> std::io::Result<usize> {
        let src = &self.entries[i];
        let shown = match src.source {
            Source::Builtin(b) => tr(BUILTINS[b].label).to_string(),
            _ => src.name.clone(),
        };
        let name = tr_fmt!("{} copy", shown);
        let text = match src.source {
            Source::Builtin(_) => format!("/* {name} */\n:root {{\n  --name: \"{name}\";\n  --base: {};\n}}\n", src.id.to_ascii_lowercase()),
            _ => themecss::set_root_decl(&src.text, "name", &format!("\"{}\"", name.replace('"', "'"))),
        };
        std::fs::create_dir_all(&self.dir)?;
        let path = self.free_path(&name);
        let file = path.file_name().map_or_else(String::new, |f| f.to_string_lossy().into_owned());
        let e = self.build(format!("user:{file}"), Source::User(path), text, None);
        self.entries.push(e);
        let k = self.entries.len() - 1;
        self.write(k)?;
        self.select(k);
        Ok(k)
    }

    /// Copies a theme file into the themes folder and selects it.
    pub fn import(&mut self, from: &Path) -> std::io::Result<usize> {
        let text = std::fs::read(from)?;
        let text = String::from_utf8_lossy(&text).into_owned();
        let stem = from.file_stem().map_or_else(|| "theme".into(), |s| s.to_string_lossy().into_owned());
        let probe = self.build(String::new(), Source::Example, text.clone(), None);
        let name = if probe.name.is_empty() { stem } else { probe.name };
        std::fs::create_dir_all(&self.dir)?;
        let path = self.free_path(&name);
        let file = path.file_name().map_or_else(String::new, |f| f.to_string_lossy().into_owned());
        let e = self.build(format!("user:{file}"), Source::User(path), text, None);
        self.entries.push(e);
        let k = self.entries.len() - 1;
        self.write(k)?;
        self.select(k);
        Ok(k)
    }

    /// A shareable file: everything that differs from the base.
    pub fn export_text(&self, i: usize) -> String {
        let e = &self.entries[i];
        if e.editable() || matches!(e.source, Source::Example) {
            return e.text.clone();
        }
        let base = super::theme::ponzik();
        themecss::export(&e.name, "ponzik", &base, &e.set.root, &[])
    }

    pub fn delete(&mut self, i: usize) -> std::io::Result<()> {
        if let Some(p) = self.entries[i].path() {
            std::fs::remove_file(p)?;
        }
        let was_active = i == self.active;
        self.entries.remove(i);
        if was_active {
            self.active = 0;
        } else if i < self.active {
            self.active -= 1;
        }
        self.refresh();
        Ok(())
    }

    /// Changes a token of the user theme `i` (`None`: back to the inherited
    /// value), in the file's text, keeping its comments. `record`: push the
    /// previous value on the token's undo stack. `save`: write the file now.
    pub fn edit(&mut self, i: usize, token: usize, value: Option<Val>, record: bool, save: bool) -> std::io::Result<()> {
        if !self.entries[i].editable() {
            return Ok(());
        }
        let def = &TOKENS[token];
        if record {
            let prev = self.entries[i].declared(def.name);
            let key = (self.entries[i].id.clone(), token);
            match self.editor.undo.iter_mut().find(|(k, _)| *k == key) {
                Some((_, stack)) => stack.push(prev),
                None => self.editor.undo.push((key, vec![prev])),
            }
        }
        let text = match value {
            Some(v) => themecss::set_root_decl(&self.entries[i].text, def.name, &themecss::value_text(def.kind, v)),
            None => themecss::remove_root_decl(&self.entries[i].text, def.name),
        };
        self.set_text(i, text, save)
    }

    fn set_text(&mut self, i: usize, text: String, save: bool) -> std::io::Result<()> {
        let e = &self.entries[i];
        let mut next = self.build(e.id.clone(), e.source.clone(), text, e.mtime);
        next.id = e.id.clone();
        self.entries[i] = next;
        if i == self.active {
            self.refresh();
        }
        if save {
            self.editor.dirty = false;
            self.write(i)
        } else {
            self.editor.dirty = true;
            Ok(())
        }
    }

    /// Writes a pending edit (end of a drag).
    pub fn flush(&mut self) -> std::io::Result<()> {
        if std::mem::take(&mut self.editor.dirty) {
            self.write(self.active)?;
        }
        Ok(())
    }

    pub fn can_undo(&self, i: usize, token: usize) -> bool {
        let key = (self.entries[i].id.clone(), token);
        self.editor.undo.iter().any(|(k, s)| *k == key && !s.is_empty())
    }

    /// Puts the token back to its value before the last change.
    pub fn undo(&mut self, i: usize, token: usize) -> std::io::Result<()> {
        let key = (self.entries[i].id.clone(), token);
        let Some(prev) = self.editor.undo.iter_mut().find(|(k, _)| *k == key).and_then(|(_, s)| s.pop()) else { return Ok(()) };
        let name = TOKENS[token].name;
        let text = match prev {
            Some(v) => themecss::set_root_decl(&self.entries[i].text, name, &v),
            None => themecss::remove_root_decl(&self.entries[i].text, name),
        };
        self.set_text(i, text, true)
    }

    /// Sets several tokens at once (generated palettes), one undo step per token.
    pub fn edit_many(&mut self, i: usize, values: &[(&str, Val)]) -> std::io::Result<()> {
        for &(name, v) in values {
            if let Some(t) = super::theme::token_index(name) {
                self.edit(i, t, Some(v), true, false)?;
            }
        }
        self.flush()
    }
}

/// Opens a file or a folder with the system's default application.
pub fn open_with_system(path: &Path) -> std::io::Result<()> {
    let mut cmd = if cfg!(windows) {
        let mut c = std::process::Command::new("cmd");
        c.arg("/C").arg("start").arg("");
        c
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else {
        std::process::Command::new("xdg-open")
    };
    cmd.arg(path).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().map(|_| ())
}

impl App {
    /// Applies the active theme's global style for this frame: colors, corners,
    /// borders, text size and animation speed.
    pub fn apply_theme_style(&mut self, c: &mut Canvas) {
        let t = self.themes.set.root;
        self.theme = t;
        c.set_style(t.radius, t.border_width);
        self.fonts.set_size_scale(t.font_scale);
        self.anim.set_speed(t.anim_speed);
        set_pattern_palette(t.patterns);
    }

    /// Runs `f` with the colors of one area of the interface (`.mixer { … }`).
    pub fn with_area<R>(&mut self, area: Area, f: impl FnOnce(&mut Self) -> R) -> R {
        let saved = self.theme;
        let t = self.themes.set.area(area);
        self.theme = t;
        set_pattern_palette(t.patterns);
        let r = f(self);
        self.theme = saved;
        set_pattern_palette(saved.patterns);
        r
    }

    /// Selects theme `i` and remembers it.
    pub fn set_theme(&mut self, i: usize) {
        self.themes.select(i);
        self.theme = self.themes.set.root;
        self.save_settings();
    }

    /// Selects a theme by id, name or file path. False if nothing matches.
    pub fn select_theme(&mut self, key: &str) -> bool {
        let path = Path::new(key);
        let found = if is_theme_file(path) && path.is_file() { self.themes.add_file(path) } else { self.themes.find(key) };
        match found {
            Some(i) => {
                self.themes.select(i);
                self.theme = self.themes.set.root;
                true
            }
            None => false,
        }
    }

    /// `PONZIK_THEME=<name|path>`: a theme for this session (captures, trials).
    pub fn theme_from_env(&mut self) {
        if let Ok(key) = std::env::var("PONZIK_THEME") {
            if !self.themes.loaded {
                self.themes.load_user();
            }
            if !self.select_theme(&key) {
                eprintln!("PONZIK_THEME: no theme named \"{key}\"");
            }
            if let Some(e) = self.themes.active().errors.first() {
                eprintln!("theme {}: {e}", self.themes.active().name);
            }
        }
    }

    /// Hot reload of the theme files; true when the interface must be redrawn.
    pub fn theme_tick(&mut self) -> bool {
        let Some(reports) = self.themes.poll(Instant::now()) else { return false };
        for (name, errors) in reports {
            if let Some(first) = errors.first() {
                let more = if errors.len() > 1 { tr_fmt!(" (+{} more)", errors.len() - 1) } else { String::new() };
                self.toast(&tr_fmt!("Theme {}: line {}: {}{}", name, first.line, first.msg, more));
            }
        }
        true
    }

    /// Display name of a theme entry (built-ins translated).
    pub fn theme_label(&self, i: usize) -> String {
        let e = &self.themes.entries[i];
        match e.source {
            Source::Builtin(b) => tr(BUILTINS[b].label).to_string(),
            _ => e.name.clone(),
        }
    }

    /// The active theme, made editable: a built-in or an example is duplicated
    /// first (they stay read-only).
    pub fn editable_theme(&mut self) -> Option<usize> {
        let i = self.themes.active;
        if self.themes.entries[i].editable() {
            return Some(i);
        }
        let label = self.theme_label(i);
        match self.themes.duplicate(i) {
            Ok(k) => {
                self.toast(&tr_fmt!("{} is read-only: editing a copy", label));
                self.save_settings();
                Some(k)
            }
            Err(e) => {
                self.toast(&tr_fmt!("Could not create the theme: {}", e.to_string()));
                None
            }
        }
    }

    pub fn duplicate_theme(&mut self, i: usize) {
        match self.themes.duplicate(i) {
            Ok(_) => {
                let name = self.themes.active().name.clone();
                self.toast(&tr_fmt!("Theme created: {}", name));
                self.save_settings();
            }
            Err(e) => self.toast(&tr_fmt!("Could not create the theme: {}", e.to_string())),
        }
    }

    pub fn delete_theme(&mut self, i: usize) {
        let name = self.theme_label(i);
        match self.themes.delete(i) {
            Ok(()) => {
                self.theme = self.themes.set.root;
                self.toast(&tr_fmt!("Theme deleted: {}", name));
                self.save_settings();
            }
            Err(e) => self.toast(&tr_fmt!("Could not delete the theme: {}", e.to_string())),
        }
    }

    pub fn import_theme(&mut self, path: &Path) {
        match self.themes.import(path) {
            Ok(k) => {
                let e = &self.themes.entries[k];
                let msg = match e.errors.first() {
                    Some(err) => tr_fmt!("Theme imported: {} · line {}: {}", e.name, err.line, err.msg),
                    None => tr_fmt!("Theme imported: {}", e.name),
                };
                self.toast(&msg);
                self.save_settings();
            }
            Err(e) => self.toast(&tr_fmt!("Could not import the theme: {}", e.to_string())),
        }
    }

    /// Writes a shareable copy in `<projects>/Themes/` and shows it.
    pub fn export_theme(&mut self, i: usize) {
        let text = self.themes.export_text(i);
        let dir = compat::projects_dir().join("Themes");
        let path = dir.join(format!("{}.ponzik-theme", compat::file_name(&self.theme_label(i))));
        let res = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, text));
        match res {
            Ok(()) => {
                self.toast(&tr_fmt!("Theme exported: {}", path.display().to_string()));
                let _ = open_with_system(&dir);
            }
            Err(e) => self.toast(&tr_fmt!("Could not export the theme: {}", e.to_string())),
        }
    }

    pub fn open_theme_file(&mut self, i: usize) {
        let Some(path) = self.themes.entries[i].path().map(Path::to_path_buf) else { return };
        if let Err(e) = open_with_system(&path) {
            self.toast(&tr_fmt!("Could not open {}: {}", path.display().to_string(), e.to_string()));
        } else {
            self.toast(tr("Edit and save: the theme updates live"));
        }
    }

    pub fn open_themes_folder(&mut self) {
        let dir = self.themes.dir.clone();
        let res = std::fs::create_dir_all(&dir).and_then(|_| open_with_system(&dir));
        if let Err(e) = res {
            self.toast(&tr_fmt!("Could not open {}: {}", dir.display().to_string(), e.to_string()));
        }
    }

    /// Changes one token of the active theme (made editable first).
    pub fn edit_token(&mut self, token: usize, value: Option<Val>, record: bool, save: bool) {
        let Some(i) = self.editable_theme() else { return };
        if let Err(e) = self.themes.edit(i, token, value, record, save) {
            self.toast(&tr_fmt!("Could not save the theme: {}", e.to_string()));
        }
        self.theme = self.themes.set.root;
    }

    /// Theme files dropped on the window are imported; the other files go on.
    pub fn take_theme_drops(&mut self, paths: &mut Vec<PathBuf>) {
        let (themes, rest): (Vec<PathBuf>, Vec<PathBuf>) = paths.drain(..).partition(|p| is_theme_file(p));
        *paths = rest;
        for p in themes {
            self.import_theme(&p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ponzik-themes-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn examples_parse_cleanly_and_are_readable() {
        let store = ThemeStore::new(temp_dir("examples"));
        for (id, _) in EXAMPLES {
            let i = store.find(id).unwrap();
            let e = &store.entries[i];
            assert!(e.errors.is_empty(), "{id}: {:?}", e.errors);
            let issues = super::super::theme::contrast_issues(&e.set.root);
            assert!(issues.is_empty(), "{id}: {issues:?}");
        }
        let light = &store.entries[store.find("Paper Light").unwrap()];
        assert!(light.set.root.is_light());
        assert!(light.set.areas[Area::Piano.index()].is_some());
        // Built-ins are found by id, translated name or slug.
        assert_eq!(store.find("glace"), Some(1));
        assert_eq!(store.find("ponzik"), Some(0));
        assert_eq!(store.entries[0].set.root, super::super::theme::ponzik());
    }

    #[test]
    fn duplicate_edit_undo_reload_delete() {
        let dir = temp_dir("edit");
        let mut store = ThemeStore::new(dir.clone());
        store.load_user();
        let k = store.duplicate(0).unwrap();
        assert_eq!(store.active, k);
        let accent = super::super::theme::token_index("accent").unwrap();
        let red = ponzik_ui::Color::hex(0xff0000);
        store.edit(k, accent, Some(Val::Color(red)), true, true).unwrap();
        assert_eq!(store.set.root.accent, red);
        // Derived tokens follow.
        assert_eq!(store.set.root.knob_arc, red);
        let path = store.entries[k].path().unwrap().to_path_buf();
        assert!(std::fs::read_to_string(&path).unwrap().contains("--accent: #ff0000;"));
        store.undo(k, accent).unwrap();
        assert_eq!(store.set.root.accent, super::super::theme::ponzik().accent);
        // Edited outside: picked up by the poll.
        std::thread::sleep(std::time::Duration::from_millis(20));
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, themecss::set_root_decl(&text, "bg", "#123456")).unwrap();
        let later = Instant::now() + std::time::Duration::from_secs(1);
        // Same mtime granularity on some file systems: force a different one.
        store.entries[k].mtime = None;
        assert!(store.poll(later).is_some());
        assert_eq!(store.set.root.bg, ponzik_ui::Color::hex(0x123456));
        // A new file appears.
        std::fs::write(dir.join("Other.css"), ":root { --name: \"Other\"; --base: amber; }").unwrap();
        assert!(store.poll(later + std::time::Duration::from_secs(1)).is_some());
        let o = store.find("Other").unwrap();
        assert_eq!(store.entries[o].set.root.accent, ponzik_ui::Color::hex(0xf5a524));
        store.delete(k).unwrap();
        assert!(!path.exists());
        assert_eq!(store.active, 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn import_and_export_round_trip() {
        let dir = temp_dir("import");
        let mut store = ThemeStore::new(dir.clone());
        let synth = store.find("synthwave").unwrap();
        let src = dir.join("shared.css");
        std::fs::write(&src, store.export_text(synth)).unwrap();
        let k = store.import(&src).unwrap();
        assert_eq!(store.entries[k].set, store.entries[synth].set);
        assert!(store.entries[k].editable());
        // A built-in exported then imported gives the same colors.
        let glace = store.find("glace").unwrap();
        std::fs::write(&src, store.export_text(glace)).unwrap();
        let k = store.import(&src).unwrap();
        assert_eq!(store.entries[k].set.root, store.entries[glace].set.root);
        let _ = std::fs::remove_dir_all(dir);
    }
}
