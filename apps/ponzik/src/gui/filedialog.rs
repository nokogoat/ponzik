//! The file dialog: Save as (Ponzik, FL Studio, Ableton Live or REAPER) and
//! Open (those, LMMS, Ardour, MusicXML and MIDI).
//!
//! One sheet for both: places on the left (home, desktop, documents, music,
//! downloads, the Ponzik projects folder, recent folders, drives), a path
//! that is clicked or typed, the folder's files with an icon per kind, sorted
//! by name, date or size, filtered by typing, and a new folder button. Open
//! also lists the recent projects. The keyboard works throughout: arrows,
//! Enter, Backspace to go up, typing to filter (or to name the file).
//!
//! The path logic (listing, filters, sorting, breadcrumbs, typed paths, the
//! file a save writes) is plain functions, tested on temporary folders.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use ponzik_i18n::{tr, tr_fmt};
use ponzik_ui::{Canvas, Color, Rect};
use ponzik_window::{Cursor, Key};

use super::Input;
use super::app::{App, Popup};
use super::compat;
use super::textedit::{EditOpts, Field};
use super::theme::*;
use super::widgets::{Icon, draw_icon, wid};

/// Width of a format chip of the Save dialog.
const CHIP_W: f32 = 136.0;

/// Recent projects kept in the settings.
pub const RECENT_MAX: usize = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Open,
    Save,
}

/// What a save writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// The project itself, everything kept.
    Ponzik,
    /// An FL Studio copy.
    Flp,
    /// An Ableton Live copy.
    Als,
    /// A REAPER copy.
    Rpp,
}

impl Format {
    pub const ALL: [Format; 4] = [Format::Ponzik, Format::Flp, Format::Als, Format::Rpp];

    pub fn ext(self) -> &'static str {
        match self {
            Format::Ponzik => "ponzik",
            Format::Flp => "flp",
            Format::Als => "als",
            Format::Rpp => "rpp",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Format::Ponzik => "Ponzik",
            Format::Flp => "FL Studio",
            Format::Als => "Ableton Live",
            Format::Rpp => "REAPER",
        }
    }

    fn kind(self) -> Kind {
        match self {
            Format::Ponzik => Kind::Ponzik,
            Format::Flp => Kind::Flp,
            Format::Als => Kind::Als,
            Format::Rpp => Kind::Rpp,
        }
    }

    /// What the file keeps, in one line.
    pub fn info(self) -> &'static str {
        match self {
            Format::Ponzik => tr("Everything is kept: devices, automation, takes, the Session view. Ctrl+S saves here next time."),
            Format::Flp => tr(
                "A copy for FL Studio: patterns, notes, channels, samples, playlist and mixer levels. Ponzik devices, effects and automation are not kept.",
            ),
            Format::Als => tr(
                "A copy for Ableton Live: tempo, one MIDI track per channel with its notes in the arrangement, markers. Instruments, effects, audio clips and automation are not kept.",
            ),
            Format::Rpp => tr(
                "A copy for REAPER: tempo changes, time signatures, markers, one track per channel with MIDI items, audio items with their fades. Instruments, effects and automation are not kept.",
            ),
        }
    }
}

/// Kind of an entry, by its extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Folder,
    Ponzik,
    Flp,
    Als,
    Rpp,
    Lmms,
    Ardour,
    MusicXml,
    Midi,
    Audio,
    Other,
}

impl Kind {
    pub fn of(path: &Path, is_dir: bool) -> Kind {
        if is_dir {
            return Kind::Folder;
        }
        let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        match ext.as_str() {
            "ponzik" => Kind::Ponzik,
            "flp" => Kind::Flp,
            "als" => Kind::Als,
            "rpp" => Kind::Rpp,
            "mmp" | "mmpz" => Kind::Lmms,
            "ardour" => Kind::Ardour,
            "musicxml" | "mxl" => Kind::MusicXml,
            "mid" | "midi" => Kind::Midi,
            _ if ponzik_engine::wav::is_audio_file(path) => Kind::Audio,
            _ => Kind::Other,
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Kind::Folder => tr("Folder"),
            Kind::Ponzik => tr("Ponzik project"),
            Kind::Flp => tr("FL Studio project"),
            Kind::Als => tr("Ableton Live set"),
            Kind::Rpp => tr("REAPER project"),
            Kind::Lmms => tr("LMMS project"),
            Kind::Ardour => tr("Ardour session"),
            Kind::MusicXml => tr("MusicXML score"),
            Kind::Midi => tr("MIDI file"),
            Kind::Audio => tr("Audio file"),
            Kind::Other => tr("File"),
        }
    }

    pub(super) fn icon(self) -> Icon {
        match self {
            Kind::Folder => Icon::Folder,
            Kind::Ponzik => Icon::Playlist,
            Kind::Flp => Icon::Rack,
            Kind::Als => Icon::Session,
            Kind::Rpp => Icon::Columns,
            Kind::Lmms => Icon::Leaf,
            Kind::Ardour => Icon::Mixer,
            Kind::MusicXml => Icon::Melody,
            Kind::Midi => Icon::Midi,
            Kind::Audio => Icon::Sample,
            Kind::Other => Icon::Document,
        }
    }

    pub(super) fn color(self, t: &Theme) -> Color {
        match self {
            Kind::Folder => t.text2,
            Kind::Ponzik => t.accent,
            Kind::Flp => t.warning,
            Kind::Als => t.frost,
            Kind::Rpp => t.accent_hot,
            Kind::Lmms => t.note,
            Kind::Ardour => t.highlight,
            Kind::MusicXml => t.meter_hot,
            Kind::Midi => t.select,
            Kind::Audio => t.wave,
            Kind::Other => t.muted,
        }
    }

    /// Files the dialog offers to open or shows next to a save.
    fn shown_in(self, mode: Mode) -> bool {
        match mode {
            Mode::Open => !matches!(self, Kind::Audio | Kind::Other),
            Mode::Save => matches!(self, Kind::Folder | Kind::Ponzik | Kind::Flp | Kind::Als | Kind::Rpp),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub kind: Kind,
    pub size: u64,
    pub modified: Option<SystemTime>,
}

/// The entries of `dir`, hidden ones (starting with a dot) left out.
pub fn list_dir(dir: &Path) -> Result<Vec<Entry>, String> {
    let rd = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut out = Vec::new();
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = e.path();
        // Links are followed: a link to a folder is a folder.
        let meta = std::fs::metadata(&path).ok();
        let is_dir = meta.as_ref().is_some_and(|m| m.is_dir());
        out.push(Entry {
            kind: Kind::of(&path, is_dir),
            size: meta.as_ref().map_or(0, |m| if m.is_dir() { 0 } else { m.len() }),
            modified: meta.and_then(|m| m.modified().ok()),
            name,
            path,
        });
    }
    Ok(out)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortBy {
    Name,
    Modified,
    Size,
}

/// The entries shown, in order: folders first, then files, by `sort`;
/// only the kinds of `mode`, and only names containing `filter` (any case).
pub fn visible(entries: &[Entry], mode: Mode, filter: &str, sort: SortBy, desc: bool) -> Vec<usize> {
    let needle = filter.trim().to_lowercase();
    let mut v: Vec<usize> = (0..entries.len())
        .filter(|&i| entries[i].kind.shown_in(mode) && (needle.is_empty() || entries[i].name.to_lowercase().contains(&needle)))
        .collect();
    v.sort_by(|&a, &b| {
        let (a, b) = (&entries[a], &entries[b]);
        let folder = (b.kind == Kind::Folder).cmp(&(a.kind == Kind::Folder));
        let by = match sort {
            SortBy::Name => natural_cmp(&a.name, &b.name),
            SortBy::Modified => a.modified.cmp(&b.modified).then_with(|| natural_cmp(&a.name, &b.name)),
            SortBy::Size => a.size.cmp(&b.size).then_with(|| natural_cmp(&a.name, &b.name)),
        };
        folder.then(if desc { by.reverse() } else { by })
    });
    v
}

/// Names compared without case, numbers by value ("Take 2" before "Take 10").
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let (mut x, mut y) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (x.peek().copied(), y.peek().copied()) {
            (None, None) => return a.cmp(b),
            (None, _) => return std::cmp::Ordering::Less,
            (_, None) => return std::cmp::Ordering::Greater,
            (Some(c), Some(d)) if c.is_ascii_digit() && d.is_ascii_digit() => {
                let mut n = String::new();
                let mut m = String::new();
                while let Some(c) = x.peek().copied().filter(char::is_ascii_digit) {
                    n.push(c);
                    x.next();
                }
                while let Some(d) = y.peek().copied().filter(char::is_ascii_digit) {
                    m.push(d);
                    y.next();
                }
                let (n, m) = (n.trim_start_matches('0'), m.trim_start_matches('0'));
                let o = n.len().cmp(&m.len()).then_with(|| n.cmp(m));
                if o != std::cmp::Ordering::Equal {
                    return o;
                }
            }
            (Some(c), Some(d)) => {
                let o = c.to_lowercase().cmp(d.to_lowercase());
                if o != std::cmp::Ordering::Equal {
                    return o;
                }
                x.next();
                y.next();
            }
        }
    }
}

/// The clickable parts of `path`, from its root: the home folder is one
/// part ("Home") when the path is inside it.
pub fn crumbs(path: &Path, home: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let rest = if home.components().count() > 1 && path.starts_with(home) {
        out.push((tr("Home folder").to_string(), home.to_path_buf()));
        path.strip_prefix(home).unwrap_or(path).to_path_buf()
    } else {
        path.to_path_buf()
    };
    let mut at = out.last().map_or_else(PathBuf::new, |c| c.1.clone());
    for comp in rest.components() {
        at.push(comp.as_os_str());
        let label = match comp {
            std::path::Component::RootDir => {
                if out.is_empty() {
                    "/".to_string()
                } else {
                    continue;
                }
            }
            std::path::Component::Prefix(p) => p.as_os_str().to_string_lossy().into_owned(),
            other => other.as_os_str().to_string_lossy().into_owned(),
        };
        // "C:" then "\": one crumb for the drive.
        if let Some(last) = out.last_mut().filter(|l| l.0.ends_with(':') && label == "/") {
            last.1 = at.clone();
            continue;
        }
        out.push((label, at.clone()));
    }
    out
}

/// A path typed in the path bar: `~` is the home folder, a relative path
/// starts from `cwd`.
pub fn typed_path(text: &str, cwd: &Path, home: &Path) -> PathBuf {
    let t = text.trim();
    if t == "~" {
        return home.to_path_buf();
    }
    if let Some(rest) = t.strip_prefix("~/").or_else(|| t.strip_prefix("~\\")) {
        return home.join(rest);
    }
    let p = Path::new(t);
    if p.is_absolute() { p.to_path_buf() } else { cwd.join(p) }
}

/// The file a save writes: `name` in `dir` with the format's extension (one
/// of the project extensions typed is replaced); characters that cannot be in
/// a file name become "_". `None` for an empty name.
pub fn save_path(dir: &Path, name: &str, format: Format) -> Option<PathBuf> {
    let mut stem: String = name.trim().chars().map(|c| if "/\\:*?\"<>|".contains(c) || c.is_control() { '_' } else { c }).collect();
    for f in Format::ALL {
        let suffix = format!(".{}", f.ext());
        if stem.to_lowercase().ends_with(&suffix) {
            stem.truncate(stem.len() - suffix.len());
        }
    }
    let stem = stem.trim().trim_end_matches('.');
    if stem.is_empty() {
        return None;
    }
    Some(dir.join(format!("{stem}.{}", format.ext())))
}

/// A place of the sidebar.
#[derive(Clone, PartialEq)]
pub struct Place {
    pub label: String,
    pub icon: Icon,
    pub path: PathBuf,
}

fn first_dir(home: &Path, names: &[&str]) -> Option<PathBuf> {
    names.iter().map(|n| home.join(n)).find(|p| p.is_dir())
}

/// Home, desktop, documents, music, downloads and the Ponzik projects folder
/// (those that exist, the projects folder always).
pub fn places(home: &Path, projects: &Path) -> Vec<Place> {
    let mut v = vec![Place { label: tr("Home folder").into(), icon: Icon::Home, path: home.to_path_buf() }];
    let mut add = |label: &str, icon, names: &[&str]| {
        if let Some(path) = first_dir(home, names) {
            v.push(Place { label: label.into(), icon, path });
        }
    };
    add(tr("Desktop"), Icon::Monitor, &["Desktop", "Bureau"]);
    add(tr("Documents"), Icon::Document, &["Documents"]);
    add(tr("Music"), Icon::Instrument, &["Music", "Musique"]);
    add(tr("Downloads"), Icon::Install, &["Downloads", "Téléchargements"]);
    v.push(Place { label: tr("Ponzik projects").into(), icon: Icon::Playlist, path: projects.to_path_buf() });
    v
}

/// Mounted drives: drive letters on Windows; the root and removable volumes
/// (`/run/media/<user>`, `/media/<user>`, `/media`, `/mnt`) elsewhere.
pub fn drives() -> Vec<Place> {
    let mut v = Vec::new();
    if cfg!(windows) {
        for letter in 'A'..='Z' {
            let root = PathBuf::from(format!("{letter}:\\"));
            if root.is_dir() {
                v.push(Place { label: format!("{letter}:"), icon: Icon::Drive, path: root });
            }
        }
        return v;
    }
    v.push(Place { label: tr("Computer").into(), icon: Icon::Drive, path: PathBuf::from("/") });
    let user = std::env::var("USER").unwrap_or_default();
    let mut roots = vec![PathBuf::from("/media"), PathBuf::from("/mnt")];
    if !user.is_empty() {
        roots.insert(0, PathBuf::from("/media").join(&user));
        roots.insert(0, PathBuf::from("/run/media").join(&user));
    }
    if cfg!(target_os = "macos") {
        roots = vec![PathBuf::from("/Volumes")];
    }
    for root in roots {
        let Ok(rd) = std::fs::read_dir(&root) else { continue };
        let mut found: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
        found.sort();
        for p in found {
            // /media/<user> is a folder of volumes, not a volume.
            if v.iter().any(|d: &Place| d.path == p) || (!user.is_empty() && p.file_name().is_some_and(|n| n == user.as_str())) {
                continue;
            }
            let label = p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned());
            v.push(Place { label, icon: Icon::Drive, path: p });
        }
    }
    v
}

/// Folders of the recent files, most recent first, without repeats.
pub fn recent_dirs(recent: &[PathBuf], max: usize) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = Vec::new();
    for f in recent {
        if let Some(d) = f.parent().filter(|d| !d.as_os_str().is_empty())
            && !v.iter().any(|x| x == d)
        {
            v.push(d.to_path_buf());
        }
        if v.len() >= max {
            break;
        }
    }
    v
}

/// `path` moved to the front of the recent list, which keeps [`RECENT_MAX`].
pub fn push_recent(recent: &mut Vec<PathBuf>, path: &Path) {
    recent.retain(|p| p != path);
    recent.insert(0, path.to_path_buf());
    recent.truncate(RECENT_MAX);
}

fn recent_file() -> PathBuf {
    compat::config_dir().join("recent.txt")
}

/// The recent projects saved by [`store_recent`]: one path per line.
pub fn load_recent() -> Vec<PathBuf> {
    let text = std::fs::read_to_string(recent_file()).unwrap_or_default();
    let mut v: Vec<PathBuf> = text.lines().map(str::trim).filter(|l| !l.is_empty()).map(PathBuf::from).collect();
    v.truncate(RECENT_MAX);
    v
}

fn store_recent(list: &[PathBuf]) {
    // Tests never touch the user's files.
    if cfg!(test) {
        return;
    }
    let path = recent_file();
    let text: String = list.iter().map(|p| format!("{}\n", p.display())).collect();
    let res = path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|_| std::fs::write(&path, text));
    if let Err(e) = res {
        eprintln!("recent projects not saved ({}): {e}", path.display());
    }
}

/// What follows once the project is saved (or not): the action that asked
/// about the unsaved changes.
#[derive(Clone, Debug, PartialEq)]
pub enum Then {
    Nothing,
    Quit,
    /// Restart into the update just installed (this program file).
    Restart(PathBuf),
    Open(PathBuf),
    NewFromTemplate(usize),
    LoadDemo(usize),
}

/// A text being typed in the dialog.
#[derive(Clone, Debug, PartialEq)]
pub enum Edit {
    None,
    /// The path bar, as text.
    Path(String),
    /// The name of a new folder.
    NewFolder(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Browse,
    Recent,
}

/// What a key or a click asks the dialog's owner to do.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// A file to open (Open) or the file to write (Save).
    Chosen(PathBuf),
    Cancel,
}

pub struct FileDialog {
    pub mode: Mode,
    pub dir: PathBuf,
    pub entries: Vec<Entry>,
    /// The folder could not be read.
    pub error: Option<String>,
    pub filter: String,
    /// Name of the file to save, without its extension.
    pub name: String,
    pub format: Format,
    pub sort: SortBy,
    pub desc: bool,
    pub selected: Option<PathBuf>,
    pub tab: Tab,
    pub edit: Edit,
    /// A file that exists and would be replaced: asked once.
    pub confirm: Option<PathBuf>,
    pub scroll: f32,
    pub then: Then,
    /// Folders visited, for Alt+Left.
    pub back: Vec<PathBuf>,
}

impl FileDialog {
    pub fn new(mode: Mode, dir: PathBuf) -> Self {
        let mut d = FileDialog {
            mode,
            dir: PathBuf::new(),
            entries: Vec::new(),
            error: None,
            filter: String::new(),
            name: String::new(),
            format: Format::Ponzik,
            sort: if mode == Mode::Open { SortBy::Modified } else { SortBy::Name },
            desc: mode == Mode::Open,
            selected: None,
            tab: Tab::Browse,
            edit: Edit::None,
            confirm: None,
            scroll: 0.0,
            then: Then::Nothing,
            back: Vec::new(),
        };
        d.go(dir);
        d
    }

    /// Lists `dir` (the closest existing parent if it is gone).
    pub fn go(&mut self, dir: PathBuf) {
        let mut dir = dir;
        while !dir.is_dir() {
            match dir.parent() {
                Some(p) if p != dir => dir = p.to_path_buf(),
                _ => break,
            }
        }
        if dir != self.dir && !self.dir.as_os_str().is_empty() {
            self.back.push(std::mem::replace(&mut self.dir, dir.clone()));
        }
        self.dir = dir;
        self.refresh();
        self.filter.clear();
        self.selected = None;
        self.scroll = 0.0;
        self.confirm = None;
    }

    pub fn refresh(&mut self) {
        match list_dir(&self.dir) {
            Ok(e) => {
                self.entries = e;
                self.error = None;
            }
            Err(e) => {
                self.entries.clear();
                self.error = Some(e);
            }
        }
    }

    pub fn up(&mut self) {
        if let Some(p) = self.dir.parent().map(Path::to_path_buf) {
            let from = self.dir.clone();
            self.go(p);
            self.selected = Some(from);
        }
    }

    pub fn shown(&self) -> Vec<usize> {
        visible(&self.entries, self.mode, &self.filter, self.sort, self.desc)
    }

    /// The file a save would write now.
    pub fn target(&self) -> Option<PathBuf> {
        save_path(&self.dir, &self.name, self.format)
    }

    fn selected_index(&self, shown: &[usize]) -> Option<usize> {
        let sel = self.selected.as_ref()?;
        shown.iter().position(|&i| &self.entries[i].path == sel)
    }

    /// Moves the selection by `step` rows.
    pub fn move_selection(&mut self, step: i32) {
        let shown = self.shown();
        if shown.is_empty() {
            return;
        }
        let at = match self.selected_index(&shown) {
            Some(i) => (i as i32 + step).clamp(0, shown.len() as i32 - 1) as usize,
            None if step < 0 => shown.len() - 1,
            None => 0,
        };
        self.pick(shown[at]);
    }

    /// Selects an entry; a save takes the name and format of a project file.
    pub fn pick(&mut self, i: usize) {
        let e = &self.entries[i];
        self.selected = Some(e.path.clone());
        if self.mode == Mode::Save
            && let Some(f) = Format::ALL.into_iter().find(|f| f.kind() == e.kind)
        {
            self.name = e.path.file_stem().map_or(String::new(), |s| s.to_string_lossy().into_owned());
            self.format = f;
        }
        self.confirm = None;
    }

    /// Enter on an entry: a folder opens, a file is chosen (Open).
    pub fn activate(&mut self, i: usize) -> Option<Outcome> {
        let e = &self.entries[i];
        if e.kind == Kind::Folder {
            let p = e.path.clone();
            self.go(p);
            return None;
        }
        match self.mode {
            Mode::Open => Some(Outcome::Chosen(e.path.clone())),
            Mode::Save => {
                self.pick(i);
                self.confirm_save()
            }
        }
    }

    /// The save button: the file, or a question first if it would replace one.
    pub fn confirm_save(&mut self) -> Option<Outcome> {
        let target = self.target()?;
        if target.exists() && self.confirm.as_ref() != Some(&target) {
            self.confirm = Some(target);
            return None;
        }
        Some(Outcome::Chosen(target))
    }

    /// The Open button: the selection, or into the selected folder.
    pub fn confirm_open(&mut self) -> Option<Outcome> {
        let shown = self.shown();
        let i = self.selected_index(&shown).map(|k| shown[k]).or_else(|| (shown.len() == 1).then(|| shown[0]))?;
        self.activate(i)
    }

    /// The filter or the name changed: the list starts over.
    pub fn typed(&mut self) {
        self.confirm = None;
        if self.mode == Mode::Open {
            self.selected = None;
            self.scroll = 0.0;
        }
    }

    /// The filter (opening) or the name (saving) being typed.
    pub fn typing_mut(&mut self) -> &mut String {
        match self.mode {
            Mode::Open => &mut self.filter,
            Mode::Save => &mut self.name,
        }
    }

    /// Keys while no text field takes them: arrows, Enter, Backspace,
    /// Escape, and typed text (the filter when opening, the name when saving).
    pub fn keys(&mut self, keys: &[(Key, ponzik_window::Modifiers)], text: &str) -> Option<Outcome> {
        let typing = match self.mode {
            Mode::Open => &mut self.filter,
            Mode::Save => &mut self.name,
        };
        let changed = !text.is_empty();
        typing.push_str(text);
        if changed {
            self.typed();
        }
        let mut out = None;
        for &(key, mods) in keys {
            match key {
                Key::Down => self.move_selection(1),
                Key::Up if mods.alt => self.up(),
                Key::Up => self.move_selection(-1),
                Key::PageDown => self.move_selection(10),
                Key::PageUp => self.move_selection(-10),
                Key::Home => self.move_selection(-100_000),
                Key::End => self.move_selection(100_000),
                Key::Left if mods.alt => {
                    if let Some(prev) = self.back.pop() {
                        self.go(prev);
                        self.back.pop();
                    }
                }
                Key::Enter => {
                    out = match self.mode {
                        Mode::Open => self.confirm_open(),
                        // A folder selected with the arrows opens; otherwise the file is saved.
                        Mode::Save => {
                            let shown = self.shown();
                            match self.selected_index(&shown).map(|k| shown[k]).filter(|&i| self.entries[i].kind == Kind::Folder) {
                                Some(i) => self.activate(i),
                                None => self.confirm_save(),
                            }
                        }
                    }
                }
                Key::Backspace => {
                    let typing = match self.mode {
                        Mode::Open => &mut self.filter,
                        Mode::Save => &mut self.name,
                    };
                    if typing.pop().is_none() && self.mode == Mode::Open {
                        self.up();
                    }
                    self.confirm = None;
                }
                Key::Escape => {
                    out = if self.confirm.take().is_some() {
                        None
                    } else if self.mode == Mode::Open && !self.filter.is_empty() {
                        self.filter.clear();
                        None
                    } else {
                        Some(Outcome::Cancel)
                    }
                }
                _ => {}
            }
            if out.is_some() {
                break;
            }
        }
        out
    }
}

/// Size of a file, in the user's units.
fn size_label(bytes: u64) -> String {
    match bytes {
        0..1_000 => tr_fmt!("{} B", bytes),
        1_000..1_000_000 => tr_fmt!("{} KB", bytes.div_ceil(1_000)),
        _ => tr_fmt!("{:.1} MB", bytes as f64 / 1e6),
    }
}

/// When a file changed: "just now", "5 min ago"… then the date.
pub(super) fn age_label(t: Option<SystemTime>) -> String {
    let Some(t) = t else { return String::new() };
    let secs = SystemTime::now().duration_since(t).map_or(0, |d| d.as_secs());
    match secs {
        0..60 => tr("just now").into(),
        60..3_600 => tr_fmt!("{} min ago", secs / 60),
        3_600..86_400 => tr_fmt!("{} h ago", secs / 3_600),
        86_400..2_592_000 => tr_fmt!("{} d ago", secs / 86_400),
        _ => {
            // Civil date from the days since 1970 (UTC is close enough for a date).
            let days = t.duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs() / 86_400) as i64;
            let z = days + 719_468;
            let era = z.div_euclid(146_097);
            let doe = z - era * 146_097;
            let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
            let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
            let mp = (5 * doy + 2) / 153;
            let d = doy - (153 * mp + 2) / 5 + 1;
            let m = if mp < 10 { mp + 3 } else { mp - 9 };
            let y = yoe + era * 400 + i64::from(m <= 2);
            format!("{y}-{m:02}-{d:02}")
        }
    }
}

const ROW_H: f32 = 40.0;

/// Id of the typing field: the filter when opening, the name when saving.
fn typing_id(mode: Mode) -> u64 {
    match mode {
        Mode::Open => wid("files-search", 1, 0),
        Mode::Save => wid("files-name", 1, 0),
    }
}

impl App {
    /// `path` becomes the most recent project.
    pub fn remember(&mut self, path: &Path) {
        push_recent(&mut self.recent_files, path);
        store_recent(&self.recent_files);
    }

    /// "Clear the list" of Open recent.
    pub fn clear_recent(&mut self) {
        self.recent_files.clear();
        store_recent(&self.recent_files);
        self.toast(tr("Recent projects cleared"));
    }

    pub fn file_dialog_open(&self) -> bool {
        self.popup.as_ref().is_some_and(|p| matches!(p.kind, Popup::Files | Popup::Unsaved))
    }

    /// The Save as dialog: in the project's folder (or the projects folder),
    /// named after the project.
    pub fn open_save_dialog(&mut self, then: Then) {
        let dir = self
            .project_path
            .as_ref()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .unwrap_or_else(compat::projects_dir);
        let _ = std::fs::create_dir_all(compat::projects_dir());
        let mut d = FileDialog::new(Mode::Save, dir);
        d.name = self.project.name.clone();
        d.then = then;
        self.files = Some(d);
        self.anim.set(wid("files-list", 0, 0), 0.0);
        self.open_popup(Popup::Files, Rect::default());
    }

    /// The Open dialog, in the folder of the last project.
    pub fn open_open_dialog(&mut self) {
        let dir = self
            .project_path
            .as_ref()
            .or(self.recent_files.first())
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .unwrap_or_else(compat::projects_dir);
        let mut d = FileDialog::new(Mode::Open, dir);
        self.recent_files.retain(|p| p.exists());
        if !self.recent_files.is_empty() && self.project_path.is_none() {
            d.tab = Tab::Recent;
        }
        self.files = Some(d);
        self.anim.set(wid("files-list", 0, 0), 0.0);
        self.open_popup(Popup::Files, Rect::default());
    }

    /// Ctrl+S: silently to the project's file, the dialog the first time.
    pub fn save(&mut self) {
        match self.project_path.clone() {
            Some(path) => {
                self.save_to(&path);
            }
            None => self.open_save_dialog(Then::Nothing),
        }
    }

    /// Writes the project to `path`, which becomes its file.
    pub fn save_to(&mut self, path: &Path) -> bool {
        self.capture_plugin_states();
        match compat::save_project(&self.project, path) {
            Ok(()) => {
                self.project_path = Some(path.to_path_buf());
                self.modified = false;
                self.mark_saved();
                self.remember(path);
                self.toast(&tr_fmt!("Saved to {}", path.display().to_string()));
                true
            }
            Err(e) => {
                self.toast(&e);
                false
            }
        }
    }

    /// Writes a copy for another program; the project keeps its own file.
    fn export_copy(&mut self, path: &Path, format: Format) -> bool {
        self.capture_plugin_states();
        let result = match format {
            Format::Flp => compat::export_flp(&self.project, path),
            Format::Als => compat::export_als(&self.project, path),
            Format::Rpp => compat::export_rpp(&self.project, path),
            Format::Ponzik => return self.save_to(path),
        };
        match result {
            Ok(()) => {
                self.toast(&tr_fmt!("{} copy written: {}", format.label(), path.display().to_string()));
                true
            }
            Err(e) => {
                self.toast(&e);
                false
            }
        }
    }

    /// Opening a file: a question first if the project has unsaved changes.
    pub fn request_open(&mut self, path: PathBuf) {
        if Kind::of(&path, false) == Kind::Midi {
            // A MIDI file goes into the current project.
            self.run_action(super::app::Action::ImportMidi(path), Rect::default());
            return;
        }
        if self.modified {
            self.ask_unsaved(Then::Open(path));
        } else {
            self.open_file(path);
        }
    }

    pub fn ask_unsaved(&mut self, then: Then) {
        self.pending = Some(then);
        self.open_popup(Popup::Unsaved, Rect::default());
    }

    /// The window's close button: true to quit now.
    pub fn close_requested(&mut self) -> bool {
        if !self.modified || self.popup.as_ref().is_some_and(|p| matches!(p.kind, Popup::Unsaved)) {
            return true;
        }
        self.ask_unsaved(Then::Quit);
        false
    }

    /// Runs what waited for the unsaved changes.
    pub fn continue_with(&mut self, then: Then) {
        match then {
            Then::Nothing => {}
            Then::Quit => self.quit_now = true,
            Then::Restart(target) => self.updates.relaunch = Some(target),
            Then::Open(path) => {
                self.open_file(path);
            }
            Then::NewFromTemplate(i) => self.new_from_template(i),
            Then::LoadDemo(i) => self.load_demo(i),
        }
    }

    /// The dialog; false when it closes.
    pub(super) fn file_dialog_popup(&mut self, c: &mut Canvas, input: &Input, screen: Rect) -> bool {
        let Some(mut d) = self.files.take() else { return false };
        let outcome = self.draw_file_dialog(c, input, screen, &mut d);
        let keep = match outcome {
            None => true,
            Some(Outcome::Cancel) => false,
            Some(Outcome::Chosen(path)) => !self.finish_dialog(&mut d, path),
        };
        if keep {
            self.files = Some(d);
        }
        keep
    }

    /// The file is chosen: opened, saved or exported. True when it is done.
    pub(super) fn finish_dialog(&mut self, d: &mut FileDialog, path: PathBuf) -> bool {
        match d.mode {
            Mode::Open => {
                self.files = None;
                self.popup = None;
                self.request_open(path);
                true
            }
            Mode::Save => {
                if !self.export_copy(&path, d.format) {
                    return false;
                }
                let then = std::mem::replace(&mut d.then, Then::Nothing);
                self.popup = None;
                // An export copy leaves the project unsaved: what waited stays asked.
                if d.format == Format::Ponzik || then == Then::Nothing {
                    self.continue_with(then);
                } else {
                    self.ask_unsaved(then);
                }
                true
            }
        }
    }

    fn draw_file_dialog(&mut self, c: &mut Canvas, input: &Input, screen: Rect, d: &mut FileDialog) -> Option<Outcome> {
        let t = self.theme;
        let open = self.anim.to(wid("popup", 0, 0), 1.0, T_HOVER);
        let mut out = None;

        // Keys: the text being edited takes them, else the list.
        match &mut d.edit {
            Edit::None => {
                if d.tab == Tab::Browse {
                    // The filter or the name has the caret; the list keeps the
                    // arrows (and Home / End while filtering).
                    let open = d.mode == Mode::Open;
                    let mut typing = std::mem::take(d.typing_mut());
                    let opts = EditOpts { keep_home_end: open };
                    let ed = self.edit_keys(typing_id(d.mode), &mut typing, input, opts);
                    *d.typing_mut() = typing;
                    if ed.changed {
                        d.typed();
                    }
                    out = d.keys(&ed.rest, "");
                } else if input.keys.iter().any(|(k, _)| *k == Key::Escape) {
                    out = Some(Outcome::Cancel);
                }
            }
            Edit::Path(text) | Edit::NewFolder(text) => {
                let ed = self.edit_keys(wid("files-edit", 0, 0), text, input, EditOpts::default());
                let done = if ed.enter {
                    Some(true)
                } else if ed.escape {
                    Some(false)
                } else {
                    None
                };
                if let Some(ok) = done {
                    let edit = std::mem::replace(&mut d.edit, Edit::None);
                    if ok {
                        self.apply_edit(d, edit);
                    }
                }
            }
        }

        c.fill_rect(screen, t.shade(130.0 * open));
        let w = 1040.0f32.min(screen.w - 40.0);
        let h = 680.0f32.min(screen.h - 40.0);
        let r = Rect::new(((screen.w - w) * 0.5).round(), ((screen.h - h) * 0.5 + (1.0 - open) * 14.0).round(), w, h);
        c.fill_rrect(r.translate(0.0, 14.0).inset(-4.0), 26.0, t.shade(110.0 * open));
        c.fill_rrect(r, 20.0, t.panel);
        c.stroke_rrect(r, 20.0, 1.0, t.hairline.alpha(1.8));

        // Header: badge, title, where, tabs, close.
        let badge = Rect::new(r.x + 22.0, r.y + 18.0, 44.0, 44.0);
        c.fill_rrect(badge, 12.0, t.accent.alpha(0.16));
        draw_icon(c, if d.mode == Mode::Save { Icon::Save } else { Icon::Folder }, badge.x + 22.0, badge.y + 22.0, 22.0, t.accent);
        let title = match d.mode {
            Mode::Save => tr("Save the project"),
            Mode::Open => tr("Open a project"),
        };
        let tx = badge.right() + 14.0;
        self.text(c, self.f.sans_med, 19.0, tx, r.y + 32.0, title, t.text);
        let sub = match (d.mode, d.tab) {
            (Mode::Save, _) => tr("Choose a place, a name and a format"),
            (Mode::Open, Tab::Browse) => tr("Ponzik and other DAWs' projects, MusicXML scores, MIDI files"),
            (Mode::Open, Tab::Recent) => tr("The projects you opened or saved lately"),
        };
        self.text_fit(c, self.f.sans, 12.5, tx, r.y + 52.0, sub, 420.0, t.text2);
        let close_r = Rect::new(r.right() - 20.0 - 36.0, r.y + 22.0, 36.0, 36.0);
        if self.icon_button(c, input, wid("files-close", 0, 0), close_r, Icon::Close, false, (tr("Close"), tr("Esc"))) {
            out = Some(Outcome::Cancel);
        }
        if d.mode == Mode::Open {
            let labels = [tr("Browse"), tr("Recent")];
            let tips = [(tr("Folders and files"), ""), (tr("Projects opened or saved lately"), "")];
            let tabs = Rect::new(close_r.x - 16.0 - 220.0, r.y + 22.0, 220.0, 36.0);
            let sel = (d.tab == Tab::Recent) as usize;
            if let Some(i) = self.segmented_tips(c, input, wid("files-tab", 0, 0), tabs, &labels, &tips, sel) {
                d.tab = if i == 1 { Tab::Recent } else { Tab::Browse };
                d.edit = Edit::None;
                self.anim.set(wid("files-list", 0, 0), 0.0);
            }
        }

        let foot_h = if d.mode == Mode::Save { 124.0 } else { 72.0 };
        let body = Rect::new(r.x, r.y + 80.0, r.w, r.h - 80.0 - foot_h);
        c.fill_rect(Rect::new(r.x + 1.0, body.y, r.w - 2.0, 1.0), t.hairline);
        let side = Rect::new(body.x + 12.0, body.y + 12.0, 212.0, body.h - 24.0);
        let main = Rect::new(side.right() + 12.0, body.y + 12.0, body.right() - side.right() - 24.0, body.h - 12.0);
        if let Some(path) = self.files_sidebar(c, input, side, d) {
            d.tab = Tab::Browse;
            d.edit = Edit::None;
            d.go(path);
            self.anim.set(wid("files-list", 0, 0), 0.0);
        }
        let picked = match d.tab {
            Tab::Browse => self.files_browser(c, input, main, d),
            Tab::Recent => self.files_recent(c, input, main),
        };
        if picked.is_some() {
            out = picked;
        }

        // Footer.
        let foot = Rect::new(r.x, r.bottom() - foot_h, r.w, foot_h);
        c.fill_rect(Rect::new(r.x + 1.0, foot.y, r.w - 2.0, 1.0), t.hairline);
        if let Some(o) = self.files_footer(c, input, foot, d) {
            out = Some(o);
        }
        if input.pressed && !r.contains(input.x, input.y) && d.confirm.is_none() {
            out = Some(Outcome::Cancel);
        }
        self.anim.set(wid("caret", 0, 0), self.time);
        out
    }

    fn apply_edit(&mut self, d: &mut FileDialog, edit: Edit) {
        match edit {
            Edit::None => {}
            Edit::Path(text) => {
                let p = typed_path(&text, &d.dir, &compat::home());
                if p.is_dir() {
                    d.go(p);
                    self.anim.set(wid("files-list", 0, 0), 0.0);
                } else if p.is_file() && d.mode == Mode::Open {
                    d.go(p.parent().map_or(p.clone(), Path::to_path_buf));
                    d.selected = Some(p);
                } else {
                    self.toast(&tr_fmt!("No folder at {}", p.display().to_string()));
                }
            }
            Edit::NewFolder(name) => {
                let name = name.trim();
                if name.is_empty() {
                    return;
                }
                let p = d.dir.join(name);
                match std::fs::create_dir(&p) {
                    Ok(()) => {
                        d.refresh();
                        d.selected = Some(p);
                    }
                    Err(e) => self.toast(&format!("{}: {e}", p.display())),
                }
            }
        }
    }

    /// Places, recent folders and drives; the place clicked.
    fn files_sidebar(&mut self, c: &mut Canvas, input: &Input, r: Rect, d: &FileDialog) -> Option<PathBuf> {
        let t = self.theme;
        let home = compat::home();
        let mut groups: Vec<(&str, Vec<Place>)> = vec![(tr("Places"), places(&home, &compat::projects_dir()))];
        let recent: Vec<Place> = recent_dirs(&self.recent_files, 4)
            .into_iter()
            .filter(|p| p.is_dir())
            .map(|p| Place { label: p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned()), icon: Icon::History, path: p })
            .collect();
        if !recent.is_empty() {
            groups.push((tr("Recent folders"), recent));
        }
        groups.push((tr("Drives"), drives()));
        let prev = c.push_clip(r);
        let mut y = r.y;
        let mut picked = None;
        for (g, (title, list)) in groups.iter().enumerate() {
            if g > 0 {
                y += 8.0;
            }
            self.text(c, self.f.sans_med, 11.0, r.x + 10.0, y + 10.0, title, t.muted);
            y += 22.0;
            for (k, place) in list.iter().enumerate() {
                let row = Rect::new(r.x, y, r.w, 34.0);
                let id = wid("files-place", g, k);
                let hov = self.hovered(input, id, row) && row.bottom() <= r.bottom();
                let hv = self.hover_anim(id, hov);
                let cur = d.tab == Tab::Browse && d.dir == place.path;
                let on = self.anim.to(id ^ 0x4f4e, cur as u8 as f32, T_HOVER);
                if hv > 0.01 || on > 0.01 {
                    c.fill_rrect(row, 10.0, t.raised.mix(t.raised_hi, on * 0.7).alpha(hv.max(on)));
                }
                if on > 0.01 {
                    c.fill_rrect(Rect::new(row.x + 3.0, row.y + 9.0, 3.0, 16.0), 1.5, t.accent.alpha(on));
                }
                let ink = t.text2.mix(t.text, hv.max(on));
                draw_icon(c, place.icon, row.x + 22.0, row.y + 17.0, 16.0, ink.mix(t.accent, on));
                self.text_fit(c, self.f.sans_med, 13.0, row.x + 40.0, row.y + 17.0, &place.label, row.w - 48.0, ink);
                if hov {
                    self.set_cursor(Cursor::Hand);
                    self.set_tip(&place.path.display().to_string(), "");
                    if input.pressed {
                        picked = Some(place.path.clone());
                    }
                }
                y += 36.0;
            }
        }
        c.set_clip(Some(prev));
        picked
    }

    /// The path bar, the tools and the list of the folder.
    fn files_browser(&mut self, c: &mut Canvas, input: &Input, r: Rect, d: &mut FileDialog) -> Option<Outcome> {
        let t = self.theme;
        let mut out = None;
        // Tools: up, path bar, new folder, search.
        let bar_y = r.y;
        let up_r = Rect::new(r.x, bar_y, 38.0, 38.0);
        if self.icon_button(c, input, wid("files-up", 0, 0), up_r, Icon::Up, false, (tr("Parent folder"), tr("Backspace"))) && d.dir.parent().is_some() {
            d.up();
            self.anim.set(wid("files-list", 0, 0), 0.0);
        }
        let search_w = 210.0;
        let nf_r = Rect::new(r.right() - search_w - 8.0 - 38.0, bar_y, 38.0, 38.0);
        if self.icon_button(c, input, wid("files-newdir", 0, 0), nf_r, Icon::NewFolder, matches!(d.edit, Edit::NewFolder(_)), (tr("New folder"), "")) {
            d.edit = Edit::NewFolder(String::new());
        }
        let path_r = Rect::new(up_r.right() + 8.0, bar_y, nf_r.x - 8.0 - up_r.right() - 8.0, 38.0);
        self.path_bar(c, input, path_r, d);
        let search = Rect::new(r.right() - search_w, bar_y, search_w, 38.0);
        self.search_field(c, input, search, d);

        // Column heads: click to sort.
        let head = Rect::new(r.x, bar_y + 50.0, r.w, 26.0);
        let cols = [(tr("Name"), SortBy::Name, head.x + 52.0), (tr("Modified"), SortBy::Modified, head.right() - 250.0), (tr("Size"), SortBy::Size, head.right() - 100.0)];
        for (k, (label, by, x)) in cols.into_iter().enumerate() {
            let lw = self.fonts.measure(self.f.sans_med, 11.5, label);
            let cell = Rect::new(x - 6.0, head.y, lw + 30.0, head.h);
            let id = wid("files-sort", k, 0);
            let hov = self.hovered(input, id, cell);
            let hv = self.hover_anim(id, hov);
            let cur = d.sort == by;
            let ink = t.muted.mix(t.text2, hv).mix(t.text, cur as u8 as f32);
            self.text(c, self.f.sans_med, 11.5, x, head.y + 13.0, label, ink);
            if cur {
                let (ax, ay) = (x + lw + 10.0, head.y + 13.0);
                let dy = if d.desc { 2.0 } else { -2.0 };
                c.stroke_polyline(&[(ax - 3.5, ay - dy), (ax, ay + dy), (ax + 3.5, ay - dy)], 1.5, t.accent);
            }
            if hov {
                self.set_cursor(Cursor::Hand);
                if input.pressed {
                    if cur {
                        d.desc = !d.desc;
                    } else {
                        d.sort = by;
                        d.desc = by != SortBy::Name;
                    }
                }
            }
        }
        c.fill_rect(Rect::new(r.x, head.bottom(), r.w, 1.0), t.hairline);

        // The list.
        let list = Rect::new(r.x, head.bottom() + 4.0, r.w, r.bottom() - head.bottom() - 4.0);
        let shown = d.shown();
        let new_row = matches!(d.edit, Edit::NewFolder(_)) as usize;
        let rows = shown.len() + new_row;
        let total = rows as f32 * ROW_H;
        let max_scroll = (total - list.h).max(0.0);
        if input.inside && list.contains(input.x, input.y) && input.wheel != 0.0 {
            d.scroll -= input.wheel * 60.0;
        }
        // Keep the selection in sight.
        if let Some(k) = d.selected_index(&shown) {
            let top = (k + new_row) as f32 * ROW_H;
            if top < d.scroll {
                d.scroll = top;
            } else if top + ROW_H > d.scroll + list.h {
                d.scroll = top + ROW_H - list.h;
            }
        }
        d.scroll = d.scroll.clamp(0.0, max_scroll);
        let scroll = self.anim.to(wid("files-scroll", 0, 0), d.scroll, T_LAYOUT);
        let enter = self.anim.to(wid("files-list", 0, 0), 1.0, T_LAYOUT);
        let prev = c.push_clip(list);
        let mut y = list.y - scroll + (1.0 - enter) * 8.0;
        if let Edit::NewFolder(name) = &d.edit {
            let row = Rect::new(list.x, y, list.w - 8.0, ROW_H - 4.0);
            c.fill_rrect(row, 10.0, t.well);
            c.stroke_rrect(row, 10.0, 1.5, t.accent.alpha(0.8));
            self.kind_badge(c, Kind::Folder, row.x + 24.0, row.y + row.h * 0.5, 1.0);
            if name.is_empty() {
                self.text(c, self.f.sans_med, 13.5, row.x + 52.0, row.y + row.h * 0.5, tr("New folder name, then Enter"), t.muted);
            } else {
                let f = Field {
                    id: wid("files-edit", 0, 0),
                    text: name,
                    area: Rect::new(row.x + 48.0, row.y, row.w - 56.0, row.h),
                    x: row.x + 52.0,
                    cy: row.y + row.h * 0.5,
                    font: self.f.sans_med,
                    size: 13.5,
                    ink: t.text,
                    caret: t.accent,
                };
                self.edit_field(c, input, &f);
            }
            y += ROW_H;
        }
        if shown.is_empty() {
            let msg = match &d.error {
                Some(e) => e.clone(),
                None if !d.filter.is_empty() => tr_fmt!("Nothing matches \"{}\"", d.filter.as_str()),
                None => tr("No projects or folders here").to_string(),
            };
            let (cx, cy) = (list.x + list.w * 0.5, list.y + list.h * 0.42);
            c.fill_circle(cx, cy - 18.0, 26.0, t.raised.alpha(0.7));
            draw_icon(c, if d.error.is_some() { Icon::Missing } else { Icon::Folder }, cx, cy - 18.0, 22.0, t.muted);
            let msg = self.fit(self.f.sans, 13.0, &msg, list.w - 40.0);
            self.text_center(c, self.f.sans, 13.0, cx, cy + 26.0, &msg, t.text2);
        }
        let sel = d.selected_index(&shown);
        let sel_y = sel.map(|k| (k + new_row) as f32 * ROW_H);
        if let Some(sy) = sel_y {
            let ay = self.anim.to(wid("files-sel", 0, 0), sy, T_LAYOUT * 0.6);
            let row = Rect::new(list.x, list.y - scroll + ay + (1.0 - enter) * 8.0, list.w - 8.0, ROW_H - 4.0);
            c.fill_rrect(row, 10.0, t.accent.alpha(0.14));
            c.fill_rrect(Rect::new(row.x + 3.0, row.y + 10.0, 3.0, row.h - 20.0), 1.5, t.accent);
        }
        let mut activate = None;
        for (k, &i) in shown.iter().enumerate() {
            if y + ROW_H < list.y || y > list.bottom() {
                y += ROW_H;
                continue;
            }
            let row = Rect::new(list.x, y, list.w - 8.0, ROW_H - 4.0);
            let id = wid("files-row", k, 0);
            let hov = self.hovered(input, id, row) && list.contains(input.x, input.y);
            let hv = self.hover_anim(id, hov);
            let is_sel = sel == Some(k);
            if hv > 0.01 && !is_sel {
                c.fill_rrect(row, 10.0, t.raised.alpha(hv * 0.9));
            }
            let e = &d.entries[i];
            let dim = d.mode == Mode::Save && e.kind != Kind::Folder && e.kind != d.format.kind();
            let a = if dim { 0.5 } else { 1.0 };
            let cy = row.y + row.h * 0.5;
            self.kind_badge(c, e.kind, row.x + 24.0, cy, a + hv * (1.0 - a));
            let name_w = (r.right() - 250.0) - (row.x + 52.0) - 16.0;
            let ink = t.text.alpha(a);
            let display = if e.kind == Kind::Folder || e.kind == Kind::Other {
                e.name.clone()
            } else {
                e.path.file_stem().map_or(e.name.clone(), |s| s.to_string_lossy().into_owned())
            };
            let nw = self.fonts.measure(self.f.sans_med, 13.5, &display).min(name_w);
            self.text_fit(c, self.f.sans_med, 13.5, row.x + 52.0, cy - 1.0, &display, name_w, ink);
            if e.kind != Kind::Folder && nw + 70.0 < name_w {
                // The kind as a small tag after the name.
                let tag = e.path.extension().map_or(String::new(), |x| x.to_string_lossy().to_ascii_uppercase());
                let tw = self.fonts.measure(self.f.mono, 10.0, &tag) + 10.0;
                let tag_r = Rect::new(row.x + 52.0 + nw + 10.0, cy - 9.0, tw, 18.0);
                c.fill_rrect(tag_r, 5.0, e.kind.color(&t).alpha(0.14 * a));
                self.text_center(c, self.f.mono, 10.0, tag_r.x + tw * 0.5, cy, &tag, e.kind.color(&t).alpha(a));
            }
            self.text(c, self.f.sans, 12.0, r.right() - 250.0, cy, &age_label(e.modified), t.text2.alpha(a));
            if e.kind == Kind::Folder {
                draw_icon(c, Icon::ChevronRight, r.right() - 26.0, cy, 13.0, t.muted.alpha(0.4 + 0.6 * hv));
            } else {
                self.text(c, self.f.mono, 11.5, r.right() - 100.0, cy, &size_label(e.size), t.text2.alpha(a));
            }
            if hov {
                self.set_cursor(Cursor::Hand);
                if input.pressed {
                    d.pick(i);
                    d.edit = Edit::None;
                    if input.double {
                        activate = Some(i);
                    }
                }
            }
            y += ROW_H;
        }
        c.set_clip(Some(prev));
        if max_scroll > 0.0 {
            let track = Rect::new(list.right() - 4.0, list.y, 4.0, list.h);
            c.fill_rrect(track, 2.0, t.well);
            let th = (list.h * list.h / total).max(28.0);
            let ty = track.y + (track.h - th) * (scroll / max_scroll);
            c.fill_rrect(Rect::new(track.x, ty, 4.0, th), 2.0, t.muted.alpha(0.8));
        }
        if let Some(i) = activate {
            out = d.activate(i);
            self.anim.set(wid("files-list", 0, 0), 0.0);
        }
        out
    }

    /// A rounded tile in the kind's colour with its icon (the Ponzik mark for
    /// Ponzik projects).
    fn kind_badge(&mut self, c: &mut Canvas, kind: Kind, cx: f32, cy: f32, alpha: f32) {
        let t = self.theme;
        let col = kind.color(&t);
        let tile = Rect::new(cx - 15.0, cy - 15.0, 30.0, 30.0);
        c.fill_rrect(tile, 9.0, col.alpha(0.16 * alpha));
        if kind == Kind::Ponzik {
            ponzik_ui::brand::draw_mark(c, tile.inset(6.0));
        } else {
            draw_icon(c, kind.icon(), cx, cy, 16.0, col.alpha(alpha));
        }
    }

    /// The folder as clickable crumbs; a click on the empty part types a path.
    fn path_bar(&mut self, c: &mut Canvas, input: &Input, r: Rect, d: &mut FileDialog) {
        let t = self.theme;
        let id = wid("files-path", 0, 0);
        let hov = self.hovered(input, id, r);
        let editing = matches!(d.edit, Edit::Path(_));
        c.fill_rrect(r, 10.0, if editing { t.well } else { t.bg_deep });
        c.stroke_rrect(r, 10.0, if editing { 1.5 } else { 1.0 }, if editing { t.accent.alpha(0.9) } else { t.hairline });
        let cy = r.y + r.h * 0.5;
        if let Edit::Path(text) = &d.edit {
            let ok_x = self.keycap(c, r.right() - 8.0, cy, tr("Enter"), 0.8);
            let f = Field {
                id: wid("files-edit", 0, 0),
                text,
                area: Rect::new(r.x + 6.0, r.y, ok_x - r.x - 12.0, r.h),
                x: r.x + 12.0,
                cy,
                font: self.f.mono,
                size: 12.5,
                ink: t.text,
                caret: t.accent,
            };
            self.edit_field(c, input, &f);
            return;
        }
        let crumbs = crumbs(&d.dir, &compat::home());
        // From the end: the last crumbs that fit, "…" before them.
        let widths: Vec<f32> = crumbs.iter().map(|(l, _)| self.fonts.measure(self.f.sans_med, 12.5, l) + 20.0).collect();
        let room = r.w - 24.0;
        let mut first = crumbs.len();
        let mut used = 0.0;
        while first > 0 && used + widths[first - 1] + 14.0 <= room {
            first -= 1;
            used += widths[first] + 14.0;
        }
        let first = first.min(crumbs.len().saturating_sub(1));
        let mut x = r.x + 6.0;
        if first > 0 {
            self.text(c, self.f.sans_med, 12.5, x + 4.0, cy, "…", t.muted);
            x += 20.0;
        }
        let mut on_crumb = false;
        let mut go = None;
        for (k, (label, path)) in crumbs.iter().enumerate().skip(first) {
            let cw = widths[k];
            let cell = Rect::new(x, r.y + 5.0, cw, r.h - 10.0);
            let cid = wid("files-crumb", k, 0);
            let ch = self.hovered(input, cid, cell);
            let hv = self.hover_anim(cid, ch);
            let last = k + 1 == crumbs.len();
            if hv > 0.01 {
                c.fill_rrect(cell, 7.0, t.raised_hi.alpha(hv));
            }
            let ink = if last { t.text } else { t.text2.mix(t.text, hv) };
            if k == 0 && label == tr("Home folder") {
                draw_icon(c, Icon::Home, cell.x + 12.0, cy, 13.0, ink);
                self.text(c, self.f.sans_med, 12.5, cell.x + 22.0, cy, label, ink);
            } else {
                self.text(c, self.f.sans_med, 12.5, cell.x + 10.0, cy, label, ink);
            }
            if ch {
                on_crumb = true;
                self.set_cursor(Cursor::Hand);
                self.set_tip(&path.display().to_string(), "");
                if input.pressed && !last {
                    go = Some(path.clone());
                }
            }
            x += cw;
            if !last {
                draw_icon(c, Icon::ChevronRight, x + 7.0, cy, 10.0, t.muted);
                x += 14.0;
            }
        }
        if let Some(p) = go {
            d.go(p);
            self.anim.set(wid("files-list", 0, 0), 0.0);
        } else if hov && !on_crumb {
            self.set_cursor(Cursor::Text);
            self.set_tip(tr("Type a path"), "");
            if input.pressed {
                d.edit = Edit::Path(d.dir.display().to_string());
            }
        }
    }

    fn search_field(&mut self, c: &mut Canvas, input: &Input, r: Rect, d: &mut FileDialog) {
        let t = self.theme;
        let id = wid("files-search", 0, 0);
        let hov = self.hovered(input, id, r);
        let hv = self.hover_anim(id, hov);
        let active = d.mode == Mode::Open && d.edit == Edit::None;
        c.fill_rrect(r, 10.0, t.bg_deep.mix(t.well, hv));
        c.stroke_rrect(r, 10.0, 1.0, if !d.filter.is_empty() { t.accent.alpha(0.7) } else { t.hairline });
        let cy = r.y + r.h * 0.5;
        draw_icon(c, Icon::Search, r.x + 18.0, cy, 14.0, if d.filter.is_empty() { t.muted } else { t.accent });
        if d.filter.is_empty() {
            let hint = if active { tr("Type to filter") } else { tr("Filter") };
            self.text_fit(c, self.f.sans, 12.5, r.x + 34.0, cy, hint, r.w - 44.0, t.muted);
        } else {
            if active {
                let f = Field {
                    id: typing_id(Mode::Open),
                    text: &d.filter,
                    area: Rect::new(r.x + 30.0, r.y, r.w - 66.0, r.h),
                    x: r.x + 34.0,
                    cy,
                    font: self.f.sans_med,
                    size: 12.5,
                    ink: t.text,
                    caret: t.accent,
                };
                self.edit_field(c, input, &f);
            } else {
                let shown = self.fit(self.f.sans_med, 12.5, &d.filter, r.w - 70.0);
                self.text(c, self.f.sans_med, 12.5, r.x + 34.0, cy, &shown, t.text);
            }
            let x_r = Rect::new(r.right() - 32.0, r.y + 5.0, 28.0, 28.0);
            if self.icon_button(c, input, wid("files-search-x", 0, 0), x_r, Icon::Close, false, (tr("Clear the filter"), tr("Esc"))) {
                d.filter.clear();
            }
        }
        if hov {
            self.set_tip(tr("Filter by name"), "");
        }
    }

    /// The recent projects, newest first.
    fn files_recent(&mut self, c: &mut Canvas, input: &Input, r: Rect) -> Option<Outcome> {
        let t = self.theme;
        let files: Vec<PathBuf> = self.recent_files.iter().filter(|p| p.exists()).cloned().collect();
        if files.is_empty() {
            let (cx, cy) = (r.x + r.w * 0.5, r.y + r.h * 0.42);
            c.fill_circle(cx, cy - 18.0, 26.0, t.raised.alpha(0.7));
            draw_icon(c, Icon::History, cx, cy - 18.0, 22.0, t.muted);
            self.text_center(c, self.f.sans, 13.0, cx, cy + 26.0, tr("Projects you open or save will show here"), t.text2);
            return None;
        }
        let enter = self.anim.to(wid("files-list", 0, 0), 1.0, T_LAYOUT);
        let mut out = None;
        let h = 58.0;
        let prev = c.push_clip(r);
        for (k, f) in files.iter().enumerate() {
            let row = Rect::new(r.x, r.y + k as f32 * h + (1.0 - enter) * 8.0, r.w - 8.0, h - 6.0);
            if row.y > r.bottom() {
                break;
            }
            let id = wid("files-recent", k, 0);
            let hov = self.hovered(input, id, row);
            let hv = self.hover_anim(id, hov);
            c.fill_rrect(row, 12.0, t.raised.alpha(0.35 + 0.65 * hv));
            let kind = Kind::of(f, false);
            let cy = row.y + row.h * 0.5;
            self.kind_badge(c, kind, row.x + 28.0, cy, 1.0);
            let name = f.file_stem().map_or(String::new(), |s| s.to_string_lossy().into_owned());
            self.text_fit(c, self.f.sans_med, 14.0, row.x + 58.0, cy - 9.0, &name, row.w - 300.0, t.text);
            let dir = f.parent().map_or(String::new(), |d| d.display().to_string());
            self.text_fit(c, self.f.sans, 11.5, row.x + 58.0, cy + 10.0, &dir, row.w - 300.0, t.text2);
            let modified = std::fs::metadata(f).and_then(|m| m.modified()).ok();
            self.text(c, self.f.sans, 12.0, row.right() - 230.0, cy, kind.label(), kind.color(&t));
            self.text(c, self.f.sans, 12.0, row.right() - 100.0, cy, &age_label(modified), t.text2);
            if hov {
                self.set_cursor(Cursor::Hand);
                self.set_tip(&f.display().to_string(), "");
                if input.pressed {
                    out = Some(Outcome::Chosen(f.clone()));
                }
            }
        }
        c.set_clip(Some(prev));
        out
    }

    /// Save: name, format, what it keeps, the buttons. Open: the buttons.
    fn files_footer(&mut self, c: &mut Canvas, input: &Input, r: Rect, d: &mut FileDialog) -> Option<Outcome> {
        let t = self.theme;
        let mut out = None;
        let pad = 24.0;
        let btn_h = 38.0;
        let by = r.bottom() - 18.0 - btn_h;
        if d.mode == Mode::Save {
            // Name and format on the first row.
            let row_y = r.y + 16.0;
            let n = Format::ALL.len() as f32;
            let chips_w = n * CHIP_W + (n - 1.0) * 8.0;
            let field = Rect::new(r.x + pad, row_y, r.w - 2.0 * pad - chips_w - 16.0, 40.0);
            c.fill_rrect(field, 11.0, t.well);
            let typing = d.edit == Edit::None;
            c.stroke_rrect(field, 11.0, 1.5, if typing { t.accent.alpha(0.85) } else { t.hairline });
            let cy = field.y + field.h * 0.5;
            let ext = format!(".{}", d.format.ext());
            let ew = self.fonts.measure(self.f.mono, 12.0, &ext);
            let room = field.w - ew - 40.0;
            let tw = if typing {
                let f = Field {
                    id: typing_id(Mode::Save),
                    text: &d.name,
                    area: Rect::new(field.x + 10.0, field.y, room + 6.0, field.h),
                    x: field.x + 14.0,
                    cy,
                    font: self.f.sans_med,
                    size: 14.0,
                    ink: t.text,
                    caret: t.accent,
                };
                self.edit_field(c, input, &f);
                self.fonts.measure(self.f.sans_med, 14.0, &d.name).min(room)
            } else {
                let shown = self.fit(self.f.sans_med, 14.0, &d.name, room);
                self.text(c, self.f.sans_med, 14.0, field.x + 14.0, cy, &shown, t.text)
            };
            self.text(c, self.f.mono, 12.0, field.x + 20.0 + tw, cy, &ext, t.muted);
            if self.hovered(input, wid("files-name", 0, 0), field) {
                self.set_cursor(Cursor::Text);
                if input.pressed {
                    d.edit = Edit::None;
                }
            }
            let mut x = field.right() + 16.0;
            for (k, f) in Format::ALL.into_iter().enumerate() {
                let chip = Rect::new(x, row_y, CHIP_W, 40.0);
                let id = wid("files-format", k, 0);
                let hov = self.hovered(input, id, chip);
                let hv = self.hover_anim(id, hov);
                let on = self.anim.to(id ^ 0x4f4e, (d.format == f) as u8 as f32, T_HOVER);
                let col = f.kind().color(&t);
                c.fill_rrect(chip, 11.0, t.raised.mix(t.raised_hi, hv * 0.6).mix(col.alpha(1.0), on * 0.16));
                c.stroke_rrect(chip, 11.0, 1.0 + on * 0.5, t.hairline.mix(col, on));
                self.kind_badge(c, f.kind(), chip.x + 22.0, chip.y + 20.0, 0.6 + 0.4 * on.max(hv));
                self.text(c, self.f.sans_med, 12.5, chip.x + 44.0, chip.y + 14.0, f.label(), t.text2.mix(t.text, on.max(hv)));
                self.text(c, self.f.mono, 10.5, chip.x + 44.0, chip.y + 28.0, &format!(".{}", f.ext()), t.muted.mix(col, on));
                if hov {
                    self.set_cursor(Cursor::Hand);
                    self.set_tip(f.info(), "");
                    if input.pressed {
                        d.format = f;
                        d.confirm = None;
                    }
                }
                x += CHIP_W + 8.0;
            }
        }
        // The info line (or the overwrite question) left of the buttons.
        let save = d.mode == Mode::Save;
        let main_label = if !save {
            tr("Open")
        } else if d.confirm.is_some() {
            tr("Replace")
        } else if d.format == Format::Ponzik {
            tr("Save")
        } else {
            tr("Export copy")
        };
        let mw = self.fonts.measure(self.f.sans_med, 13.0, main_label) + 56.0;
        let main_r = Rect::new(r.right() - pad - mw, by, mw, btn_h);
        let cancel = tr("Cancel");
        let cw = self.fonts.measure(self.f.sans_med, 13.0, cancel) + 40.0;
        let cancel_r = Rect::new(main_r.x - 10.0 - cw, by, cw, btn_h);
        let info_w = cancel_r.x - r.x - pad - 16.0;
        let cy = by + btn_h * 0.5;
        if let Some(target) = &d.confirm {
            let name = target.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned());
            let strip = Rect::new(r.x + pad, by - 2.0, info_w, btn_h + 4.0);
            c.fill_rrect(strip, 11.0, t.warning.alpha(0.12));
            draw_icon(c, Icon::Missing, strip.x + 20.0, cy, 15.0, t.warning);
            let q = tr_fmt!("{} already exists. Replace it?", name);
            self.text_fit(c, self.f.sans_med, 12.5, strip.x + 38.0, cy, &q, strip.w - 48.0, t.text);
        } else if save {
            let col = d.format.kind().color(&t);
            c.fill_circle(r.x + pad + 5.0, cy - 7.0, 3.0, col);
            let lines = self.wrap(self.f.sans, 12.0, d.format.info(), info_w - 18.0);
            for (k, line) in lines.iter().take(2).enumerate() {
                self.text(c, self.f.sans, 12.0, r.x + pad + 16.0, cy - 7.0 + k as f32 * 16.0, line, t.text2);
            }
        } else {
            let mut x = r.x + pad;
            for kind in [Kind::Ponzik, Kind::Flp, Kind::Als, Kind::Rpp, Kind::Lmms, Kind::Ardour, Kind::MusicXml, Kind::Midi] {
                self.kind_badge(c, kind, x + 15.0, cy, 0.9);
                x += 36.0;
            }
            let hint = tr("Arrows to move · Enter to open · Backspace for the parent folder");
            self.text_fit(c, self.f.sans, 12.0, x + 8.0, cy, hint, cancel_r.x - x - 24.0, t.text2);
        }
        // Cancel answers the overwrite question first, then closes.
        if self.pill_button(c, input, wid("files-cancel", 0, 0), cancel_r, Icon::Close, cancel, false) && d.confirm.take().is_none() {
            out = Some(Outcome::Cancel);
        }
        let enabled = if save { d.target().is_some() } else { d.tab == Tab::Recent || !d.shown().is_empty() };
        let icon = if save { if d.format == Format::Ponzik { Icon::Save } else { Icon::Export } } else { Icon::Folder };
        if self.pill_button(c, input, wid("files-ok", 0, 0), main_r, icon, main_label, enabled) && enabled {
            out = match d.mode {
                Mode::Save => d.confirm_save(),
                Mode::Open => d.confirm_open(),
            };
        }
        out
    }

    /// "Save your changes?": Save, Don't save, Cancel. False when it closes.
    pub(super) fn unsaved_popup(&mut self, c: &mut Canvas, input: &Input, screen: Rect) -> bool {
        let t = self.theme;
        let open = self.anim.to(wid("popup", 0, 0), 1.0, T_HOVER);
        c.fill_rect(screen, t.shade(130.0 * open));
        let w = 500.0f32.min(screen.w - 40.0);
        let h = 184.0;
        let r = Rect::new(((screen.w - w) * 0.5).round(), (screen.h * 0.3 + (1.0 - open) * 12.0).round(), w, h);
        c.fill_rrect(r.translate(0.0, 12.0).inset(-4.0), 24.0, t.shade(110.0 * open));
        c.fill_rrect(r, 20.0, t.panel);
        c.stroke_rrect(r, 20.0, 1.0, t.hairline.alpha(1.8));
        let badge = Rect::new(r.x + 24.0, r.y + 24.0, 44.0, 44.0);
        c.fill_rrect(badge, 12.0, t.warning.alpha(0.16));
        draw_icon(c, Icon::Save, badge.x + 22.0, badge.y + 22.0, 20.0, t.warning);
        let tx = badge.right() + 16.0;
        self.text_fit(c, self.f.sans_med, 18.0, tx, r.y + 36.0, tr("Save your changes?"), r.right() - tx - 24.0, t.text);
        let then = self.pending.clone().unwrap_or(Then::Nothing);
        let sub = match then {
            Then::Quit | Then::Restart(_) => tr_fmt!("\"{}\" has unsaved changes: they are lost if Ponzik closes now.", self.project.name.as_str()),
            _ => tr_fmt!("\"{}\" has unsaved changes: they are lost if another project replaces it.", self.project.name.as_str()),
        };
        let lines = self.wrap(self.f.sans, 13.0, &sub, r.right() - tx - 24.0);
        for (k, l) in lines.iter().take(2).enumerate() {
            self.text(c, self.f.sans, 13.0, tx, r.y + 60.0 + k as f32 * 18.0, l, t.text2);
        }
        let by = r.bottom() - 24.0 - 38.0;
        let save = tr("Save");
        let sw = self.fonts.measure(self.f.sans_med, 13.0, save) + 56.0;
        let save_r = Rect::new(r.right() - 24.0 - sw, by, sw, 38.0);
        let discard = tr("Don't save");
        let dw = self.fonts.measure(self.f.sans_med, 13.0, discard) + 48.0;
        let discard_r = Rect::new(save_r.x - 10.0 - dw, by, dw, 38.0);
        let cancel = tr("Cancel");
        let cw = self.fonts.measure(self.f.sans_med, 13.0, cancel) + 40.0;
        let cancel_r = Rect::new(r.x + 24.0, by, cw, 38.0);
        let mut keep = true;
        if self.pill_button(c, input, wid("unsaved-cancel", 0, 0), cancel_r, Icon::Close, cancel, false) {
            self.pending = None;
            keep = false;
        }
        if self.pill_button(c, input, wid("unsaved-discard", 0, 0), discard_r, Icon::Trash, discard, false) {
            self.pending = None;
            self.popup = None;
            self.continue_with(then.clone());
            return false;
        }
        let enter = input.keys.iter().any(|(k, _)| *k == Key::Enter);
        if self.pill_button(c, input, wid("unsaved-save", 0, 0), save_r, Icon::Save, save, true) || enter {
            self.pending = None;
            self.popup = None;
            match self.project_path.clone() {
                Some(path) => {
                    if self.save_to(&path) {
                        self.continue_with(then);
                    }
                }
                None => self.open_save_dialog(then),
            }
            return false;
        }
        if input.keys.iter().any(|(k, _)| *k == Key::Escape) || (input.pressed && !r.contains(input.x, input.y)) {
            self.pending = None;
            keep = false;
        }
        keep
    }

    /// Captures: `PONZIK_FILEDIALOG=save|open|recent|unsaved[:folder][,flp|als|rpp][,sel:n][,q:text][,confirm][,newdir][,path]`.
    pub fn screenshot_file_dialog(&mut self, spec: &str) {
        let mut parts = spec.split(',').map(str::trim);
        let head = parts.next().unwrap_or("save");
        let (what, dir) = head.split_once(':').map_or((head, None), |(a, b)| (a, Some(PathBuf::from(b))));
        if what == "unsaved" {
            self.modified = true;
            self.ask_unsaved(Then::Quit);
            return;
        }
        if what == "recent" {
            if let Some(dir) = &dir {
                let mut found: Vec<Entry> = list_dir(dir).unwrap_or_default().into_iter().filter(|e| e.kind != Kind::Folder && e.kind.shown_in(Mode::Open)).collect();
                found.sort_by_key(|e| std::cmp::Reverse(e.modified));
                self.recent_files = found.into_iter().map(|e| e.path).collect();
            }
            self.open_open_dialog();
            if let Some(d) = self.files.as_mut() {
                d.tab = Tab::Recent;
            }
            return;
        }
        if what == "open" {
            self.open_open_dialog();
        } else {
            self.open_save_dialog(Then::Nothing);
        }
        let Some(d) = self.files.as_mut() else { return };
        d.tab = Tab::Browse;
        if let Some(dir) = dir {
            d.go(dir);
        }
        for p in parts {
            match p {
                "flp" => d.format = Format::Flp,
                "als" => d.format = Format::Als,
                "rpp" => d.format = Format::Rpp,
                "confirm" => {
                    if let Some(i) = d.shown().into_iter().find(|&i| d.entries[i].kind == Kind::Ponzik) {
                        d.pick(i);
                        d.confirm_save();
                    }
                }
                "newdir" => d.edit = Edit::NewFolder("Remixes".into()),
                "path" => d.edit = Edit::Path(d.dir.display().to_string()),
                _ => {
                    if let Some(n) = p.strip_prefix("sel:").and_then(|n| n.parse::<usize>().ok()) {
                        let shown = d.shown();
                        if let Some(&i) = shown.get(n) {
                            d.pick(i);
                        }
                    } else if let Some(q) = p.strip_prefix("q:") {
                        d.filter = q.to_string();
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh folder under the system's temporary folder.
    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ponzik-files-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn touch(dir: &Path, name: &str, bytes: usize) {
        std::fs::write(dir.join(name), vec![b'x'; bytes]).unwrap();
    }

    #[test]
    fn listing_filters_and_sorts() {
        let d = temp_dir("list");
        std::fs::create_dir(d.join("Beats")).unwrap();
        std::fs::create_dir(d.join("archive")).unwrap();
        std::fs::create_dir(d.join(".hidden")).unwrap();
        touch(&d, "Song 10.ponzik", 300);
        touch(&d, "Song 2.ponzik", 100);
        touch(&d, "remix.FLP", 5000);
        touch(&d, "live set.als", 50);
        touch(&d, "groove.mid", 10);
        touch(&d, "notes.txt", 10);
        touch(&d, "kick.wav", 10);
        let e = list_dir(&d).unwrap();
        assert_eq!(e.len(), 9, "the dot folder is hidden");
        let names = |v: Vec<usize>| v.into_iter().map(|i| e[i].name.clone()).collect::<Vec<_>>();
        // Open: folders first, natural order, project and MIDI files only.
        assert_eq!(
            names(visible(&e, Mode::Open, "", SortBy::Name, false)),
            ["archive", "Beats", "groove.mid", "live set.als", "remix.FLP", "Song 2.ponzik", "Song 10.ponzik"]
        );
        // Save: no MIDI file.
        assert!(!names(visible(&e, Mode::Save, "", SortBy::Name, false)).contains(&"groove.mid".to_string()));
        // Size, largest first; folders stay on top.
        assert_eq!(names(visible(&e, Mode::Open, "", SortBy::Size, true))[2], "remix.FLP");
        // The filter ignores case and keeps matching folders.
        assert_eq!(names(visible(&e, Mode::Open, "SONG", SortBy::Name, false)), ["Song 2.ponzik", "Song 10.ponzik"]);
        assert_eq!(names(visible(&e, Mode::Open, "be", SortBy::Name, false)), ["Beats"]);
        assert_eq!(Kind::of(&d.join("remix.FLP"), false), Kind::Flp);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn natural_order() {
        let mut v = vec!["Take 10", "take 2", "Take 1", "Alpha", "take 02b"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, ["Alpha", "Take 1", "take 2", "take 02b", "Take 10"]);
    }

    #[test]
    fn breadcrumbs() {
        let home = Path::new("/home/ana");
        let c = crumbs(Path::new("/home/ana/Music/Ponzik"), home);
        let labels: Vec<&str> = c.iter().map(|c| c.0.as_str()).collect();
        assert_eq!(labels, [tr("Home folder"), "Music", "Ponzik"]);
        assert_eq!(c[1].1, Path::new("/home/ana/Music"));
        assert_eq!(c[2].1, Path::new("/home/ana/Music/Ponzik"));
        let c = crumbs(Path::new("/mnt/usb/sets"), home);
        let labels: Vec<&str> = c.iter().map(|c| c.0.as_str()).collect();
        assert_eq!(labels, ["/", "mnt", "usb", "sets"]);
        assert_eq!(c[0].1, Path::new("/"));
        assert_eq!(c[3].1, Path::new("/mnt/usb/sets"));
        assert_eq!(crumbs(Path::new("/"), home).len(), 1);
        // A home of "/" is not a place of its own.
        assert_eq!(crumbs(Path::new("/tmp"), Path::new("/"))[0].0, "/");
    }

    #[test]
    fn typed_paths() {
        let (cwd, home) = (Path::new("/data/music"), Path::new("/home/ana"));
        assert_eq!(typed_path("~", cwd, home), home);
        assert_eq!(typed_path(" ~/Desktop ", cwd, home), Path::new("/home/ana/Desktop"));
        assert_eq!(typed_path("/etc", cwd, home), Path::new("/etc"));
        assert_eq!(typed_path("sets/old", cwd, home), Path::new("/data/music/sets/old"));
    }

    #[test]
    fn save_paths_get_one_extension() {
        let dir = Path::new("/p");
        assert_eq!(save_path(dir, "My song", Format::Ponzik).unwrap(), Path::new("/p/My song.ponzik"));
        assert_eq!(save_path(dir, "My song.ponzik", Format::Flp).unwrap(), Path::new("/p/My song.flp"));
        assert_eq!(save_path(dir, "Mix.ALS", Format::Als).unwrap(), Path::new("/p/Mix.als"));
        assert_eq!(save_path(dir, "a/b: c?", Format::Ponzik).unwrap(), Path::new("/p/a_b_ c_.ponzik"));
        assert_eq!(save_path(dir, "v1.2", Format::Ponzik).unwrap(), Path::new("/p/v1.2.ponzik"));
        assert!(save_path(dir, "  ", Format::Ponzik).is_none());
        assert!(save_path(dir, ".flp", Format::Flp).is_none());
    }

    #[test]
    fn keyboard_navigation_and_overwrite() {
        let d = temp_dir("keys");
        std::fs::create_dir(d.join("Sets")).unwrap();
        touch(&d.join("Sets"), "inside.ponzik", 1);
        touch(&d, "a.ponzik", 1);
        touch(&d, "b.flp", 1);
        let none = ponzik_window::Modifiers::default();
        let alt = ponzik_window::Modifiers { alt: true, ..Default::default() };
        let mut f = FileDialog::new(Mode::Open, d.clone());
        f.sort = SortBy::Name;
        f.desc = false;
        // Down selects the first row (the folder), Enter goes in.
        assert_eq!(f.keys(&[(Key::Down, none)], ""), None);
        assert_eq!(f.selected.as_deref(), Some(d.join("Sets").as_path()));
        assert_eq!(f.keys(&[(Key::Enter, none)], ""), None);
        assert_eq!(f.dir, d.join("Sets"));
        // Backspace with no filter goes up, the folder we came from selected.
        f.keys(&[(Key::Backspace, none)], "");
        assert_eq!(f.dir, d);
        assert_eq!(f.selected.as_deref(), Some(d.join("Sets").as_path()));
        // Typing filters; Enter opens the only match.
        f.keys(&[], "b.f");
        assert_eq!(f.shown().len(), 1);
        assert_eq!(f.keys(&[(Key::Enter, none)], ""), Some(Outcome::Chosen(d.join("b.flp"))));
        // Escape clears the filter, then closes.
        assert_eq!(f.keys(&[(Key::Escape, none)], ""), None);
        assert!(f.filter.is_empty());
        assert_eq!(f.keys(&[(Key::Escape, none)], ""), Some(Outcome::Cancel));
        // Alt+Up and Alt+Left.
        f.go(d.join("Sets"));
        f.keys(&[(Key::Up, alt)], "");
        assert_eq!(f.dir, d);
        f.keys(&[(Key::Left, alt)], "");
        assert_eq!(f.dir, d.join("Sets"));

        // Save: typing names the file; an existing file asks once.
        let mut s = FileDialog::new(Mode::Save, d.clone());
        s.keys(&[], "a");
        assert_eq!(s.target(), Some(d.join("a.ponzik")));
        assert_eq!(s.keys(&[(Key::Enter, none)], ""), None);
        assert_eq!(s.confirm, Some(d.join("a.ponzik")));
        assert_eq!(s.keys(&[(Key::Enter, none)], ""), Some(Outcome::Chosen(d.join("a.ponzik"))));
        // Picking an FL Studio file takes its name and format.
        let i = s.entries.iter().position(|e| e.name == "b.flp").unwrap();
        s.pick(i);
        assert_eq!((s.name.as_str(), s.format), ("b", Format::Flp));
        // A new name saves at once.
        s.name = "fresh".into();
        assert_eq!(s.confirm_save(), Some(Outcome::Chosen(d.join("fresh.flp"))));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_missing_folder_falls_back_to_its_parent() {
        let d = temp_dir("gone");
        let f = FileDialog::new(Mode::Open, d.join("not/there"));
        assert_eq!(f.dir, d);
        assert!(f.error.is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn recents() {
        let mut r = Vec::new();
        for i in 0..20 {
            push_recent(&mut r, &PathBuf::from(format!("/m/{}/s{i}.ponzik", i % 3)));
        }
        push_recent(&mut r, Path::new("/m/0/s3.ponzik"));
        assert_eq!(r.len(), RECENT_MAX);
        assert_eq!(r[0], Path::new("/m/0/s3.ponzik"));
        assert_eq!(r.iter().filter(|p| *p == Path::new("/m/0/s3.ponzik")).count(), 1);
        assert_eq!(recent_dirs(&r, 5), [Path::new("/m/0"), Path::new("/m/1"), Path::new("/m/2")]);
    }

    #[test]
    fn places_exist_and_the_projects_folder_is_always_there() {
        let d = temp_dir("places");
        std::fs::create_dir(d.join("Musique")).unwrap();
        let p = places(&d, Path::new("/x/Ponzik"));
        assert_eq!(p[0].path, d);
        assert!(p.iter().any(|p| p.path == d.join("Musique")));
        assert!(!p.iter().any(|p| p.icon == Icon::Monitor), "no desktop folder here");
        assert_eq!(p.last().unwrap().path, Path::new("/x/Ponzik"));
        let _ = std::fs::remove_dir_all(&d);
    }

    fn test_app() -> App {
        let project = ponzik_project::demo_project();
        let (_engine, handle) = project.build_engine(48_000.0, &|_| None);
        App::new(handle, project, 48_000)
    }

    #[test]
    fn the_first_save_asks_where_the_next_ones_do_not() {
        let d = temp_dir("first-save");
        let mut app = test_app();
        app.modified = true;
        // Never saved: Ctrl+S opens Save as, named after the project.
        app.save();
        assert!(matches!(app.popup.as_ref().map(|p| &p.kind), Some(Popup::Files)));
        let dlg = app.files.as_mut().unwrap();
        assert_eq!((dlg.mode, dlg.format), (Mode::Save, Format::Ponzik));
        assert_eq!(dlg.name, app.project.name);
        // Chosen: the project now lives in that file.
        dlg.go(d.clone());
        let target = dlg.target().unwrap();
        let mut dlg = app.files.take().unwrap();
        assert!(app.finish_dialog(&mut dlg, target.clone()));
        assert_eq!(app.project_path.as_deref(), Some(target.as_path()));
        assert!(!app.modified && target.is_file());
        assert_eq!(app.recent_files.first(), Some(&target));
        // The next Ctrl+S writes there without a question.
        app.popup = None;
        app.project.bpm = 99.0;
        app.modified = true;
        app.save();
        assert!(app.popup.is_none() && !app.modified);
        assert_eq!(ponzik_project::load_from_file(&target).unwrap().bpm, 99.0);
        // An FL Studio copy leaves the project's own file alone.
        let mut copy = FileDialog::new(Mode::Save, d.clone());
        copy.name = "for fl".into();
        copy.format = Format::Flp;
        let flp = copy.target().unwrap();
        assert!(app.finish_dialog(&mut copy, flp.clone()));
        assert!(flp.is_file());
        assert_eq!(app.project_path.as_deref(), Some(target.as_path()));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn unsaved_changes_are_asked_about() {
        let mut app = test_app();
        app.modified = false;
        assert!(app.close_requested(), "nothing to lose: quits");
        app.modified = true;
        assert!(!app.close_requested());
        assert!(matches!(app.popup.as_ref().map(|p| &p.kind), Some(Popup::Unsaved)));
        assert_eq!(app.pending, Some(Then::Quit));
        // "Don't save" goes on with what waited.
        app.continue_with(Then::Quit);
        assert!(app.quit_requested());
        // Opening another project asks too.
        let mut app = test_app();
        app.modified = true;
        app.request_open(PathBuf::from("/nowhere/x.ponzik"));
        assert_eq!(app.pending, Some(Then::Open(PathBuf::from("/nowhere/x.ponzik"))));
        // Restarting into an update: "Don't save" restarts all the same.
        let mut app = test_app();
        app.modified = true;
        app.ask_unsaved(Then::Restart(PathBuf::from("/nowhere/Ponzik.AppImage")));
        assert!(!app.quit_requested());
        app.continue_with(Then::Restart(PathBuf::from("/nowhere/Ponzik.AppImage")));
        assert!(app.quit_requested());
        assert!(app.modified, "nothing was saved");
    }

    #[test]
    fn labels() {
        assert!(age_label(Some(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(86_400 * 365))).starts_with("1971-01-01"));
        assert_eq!(age_label(Some(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000))), "2023-11-14");
        assert!(size_label(999).contains("999"));
        assert!(size_label(2_500).contains('3'));
    }
}
