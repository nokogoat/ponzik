//! Browser tree: the items of a browser tab grouped in collapsible folders
//! (sounds by library folder, instruments and effects by category, plugins
//! by kind), the rows shown for the folders open, the arrow keys that walk
//! them, and short names for the sounds (`Kick - Punchy House 01` under
//! Kicks reads `Punchy House 01`, a tempo or a key becomes a badge).
//!
//! The model is pure; the drawing lives in `App::browser_tree`.

use ponzik_i18n::{tr, tr_fmt};
use ponzik_ui::{Canvas, Color, Rect};

use super::Input;
use super::app::{App, BrowserEntry, RowHit, plugin_badge};
use super::browser::{BrowserPrefs, Tag, TagSet, item_key};
use super::dnd::Payload;
use super::theme::T_LAYOUT;
use super::theme::T_HOVER;
use super::widgets::{Icon, draw_icon, wid};

/// Top folder of the samples found outside the library (shown translated).
pub const USER_ROOT: &str = "Your samples";

/// A folder: its stable key (tab prefix and path, `s/Drums/Kicks`), its
/// name as found, where it sits and what it holds.
#[derive(Clone, Debug, PartialEq)]
pub struct Folder {
    pub key: String,
    pub name: String,
    pub depth: usize,
    pub parent: Option<usize>,
    /// Its top-level folder (itself at depth 0): gives the color and icon.
    pub top: usize,
    pub folders: Vec<usize>,
    /// Entries (indices given to [`Tree::build`]), in the order given.
    pub items: Vec<usize>,
    /// Items in it and below.
    pub count: usize,
}

/// The folders of one tab. Folders keep the order of their first item.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tree {
    pub folders: Vec<Folder>,
    pub roots: Vec<usize>,
    /// Items in no folder, listed after the folders.
    pub loose: Vec<usize>,
}

/// What a row shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Node {
    Folder(usize),
    Item(usize),
}

/// A row of the tree, `reveal` from 0 (folded away) to 1 (fully shown):
/// the product of its ancestors' opening.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TreeRow {
    pub node: Node,
    pub depth: usize,
    pub reveal: f32,
    /// Folder it belongs to.
    pub parent: Option<usize>,
}

impl Tree {
    /// `items`: entry index and its folder path (empty: no folder). `prefix`
    /// starts every key, so that tabs never share an open state.
    pub fn build(prefix: &str, items: impl IntoIterator<Item = (usize, Vec<String>)>) -> Tree {
        let mut tree = Tree::default();
        for (entry, path) in items {
            let mut parent: Option<usize> = None;
            let mut key = prefix.to_string();
            for (depth, name) in path.iter().filter(|n| !n.is_empty()).enumerate() {
                key.push('/');
                key.push_str(name);
                let siblings = match parent {
                    Some(p) => &tree.folders[p].folders,
                    None => &tree.roots,
                };
                let found = siblings.iter().copied().find(|&f| tree.folders[f].name == *name);
                let f = match found {
                    Some(f) => f,
                    None => {
                        let f = tree.folders.len();
                        let top = parent.map_or(f, |p| tree.folders[p].top);
                        tree.folders.push(Folder {
                            key: key.clone(),
                            name: name.clone(),
                            depth,
                            parent,
                            top,
                            folders: Vec::new(),
                            items: Vec::new(),
                            count: 0,
                        });
                        match parent {
                            Some(p) => tree.folders[p].folders.push(f),
                            None => tree.roots.push(f),
                        }
                        f
                    }
                };
                tree.folders[f].count += 1;
                parent = Some(f);
            }
            match parent {
                Some(p) => tree.folders[p].items.push(entry),
                None => tree.loose.push(entry),
            }
        }
        tree
    }

    pub fn find(&self, key: &str) -> Option<usize> {
        self.folders.iter().position(|f| f.key == key)
    }

    /// `f` and the folders above it, top first.
    pub fn lineage(&self, f: usize) -> Vec<usize> {
        let mut v = vec![f];
        let mut cur = self.folders[f].parent;
        while let Some(p) = cur {
            v.push(p);
            cur = self.folders[p].parent;
        }
        v.reverse();
        v
    }

    /// Rows in display order: a folder, then its subfolders, then its items.
    /// `open(f)` says how open folder `f` is (0 to 1, animated); the rows
    /// of a folder closing stay listed until they are folded away.
    pub fn rows(&self, open: &dyn Fn(usize) -> f32) -> Vec<TreeRow> {
        let mut out = Vec::new();
        for &f in &self.roots {
            self.push_rows(f, 1.0, open, &mut out);
        }
        out.extend(self.loose.iter().map(|&e| TreeRow { node: Node::Item(e), depth: 0, reveal: 1.0, parent: None }));
        out
    }

    fn push_rows(&self, f: usize, reveal: f32, open: &dyn Fn(usize) -> f32, out: &mut Vec<TreeRow>) {
        let folder = &self.folders[f];
        out.push(TreeRow { node: Node::Folder(f), depth: folder.depth, reveal, parent: folder.parent });
        let o = open(f).clamp(0.0, 1.0);
        if o <= 0.0 || reveal <= 0.0 {
            return;
        }
        let inner = reveal * o;
        for &sub in &folder.folders {
            self.push_rows(sub, inner, open, out);
        }
        out.extend(folder.items.iter().map(|&e| TreeRow { node: Node::Item(e), depth: folder.depth + 1, reveal: inner, parent: Some(f) }));
    }
}

// --- Keyboard -------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavKey {
    Up,
    Down,
    Left,
    Right,
    Enter,
}

/// What a key does to the tree.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Nav {
    /// Row picked after the key.
    pub sel: Option<usize>,
    /// A folder to open (true) or close (false).
    pub fold: Option<(usize, bool)>,
    /// Enter on an item: add it.
    pub activate: Option<usize>,
}

/// Arrow keys in the tree, file-manager style: Up and Down walk the rows,
/// Right opens a folder then enters it, Left closes it then goes up to the
/// parent, Enter opens or closes a folder and adds an item.
/// `rows` are the rows of the open folders; `is_open(f)` their state.
pub fn nav(rows: &[TreeRow], sel: Option<usize>, key: NavKey, is_open: &dyn Fn(usize) -> bool) -> Nav {
    let mut out = Nav { sel, ..Nav::default() };
    if rows.is_empty() {
        out.sel = None;
        return out;
    }
    let last = rows.len() - 1;
    let Some(s) = sel.filter(|&s| s <= last) else {
        out.sel = Some(if key == NavKey::Up { last } else { 0 });
        return out;
    };
    let row = rows[s];
    match (key, row.node) {
        (NavKey::Up, _) => out.sel = Some(s.saturating_sub(1)),
        (NavKey::Down, _) => out.sel = Some((s + 1).min(last)),
        (NavKey::Right, Node::Folder(f)) if !is_open(f) => out.fold = Some((f, true)),
        (NavKey::Right, Node::Folder(_)) => {
            if rows.get(s + 1).is_some_and(|r| r.depth > row.depth) {
                out.sel = Some(s + 1);
            }
        }
        (NavKey::Left, Node::Folder(f)) if is_open(f) => out.fold = Some((f, false)),
        (NavKey::Left, _) => {
            if let Some(p) = row.parent
                && let Some(i) = rows.iter().position(|r| r.node == Node::Folder(p))
            {
                out.sel = Some(i);
            }
        }
        (NavKey::Enter, Node::Folder(f)) => out.fold = Some((f, !is_open(f))),
        (NavKey::Enter, Node::Item(e)) => out.activate = Some(e),
        (NavKey::Right, Node::Item(_)) => {}
    }
    out
}

// --- Names ----------------------------------------------------------------------

/// How a sound is shown in its folder.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Label {
    pub title: String,
    /// Tempo found in the name (`124 BPM`, `90bpm`).
    pub bpm: Option<u16>,
    /// Key, note or chord ending the name (`- Cm7`, `- F#1`).
    pub key: Option<String>,
}

fn singular(w: &str) -> &str {
    if w.len() > 3 && w.ends_with('s') && !w.ends_with("ss") { &w[..w.len() - 1] } else { w }
}

/// `word` names the same thing as the folder `folder` (`Kick`/`Kicks`, `Perc`/`Percussion`).
fn names_folder(word: &str, folder: &str) -> bool {
    let (w, f) = (word.to_lowercase(), folder.to_lowercase());
    !w.is_empty() && (w == f || singular(&w) == singular(&f) || (w.len() >= 4 && f.starts_with(&w)))
}

/// `C`, `F#1`, `Eb`, `Cm7`, `Fmaj9`, `Am`, `Dsus4`…: a note or a chord.
pub fn is_key_name(s: &str) -> bool {
    let b = s.as_bytes();
    if b.is_empty() || !(b'A'..=b'G').contains(&b[0]) {
        return false;
    }
    let mut rest = &s[1..];
    if let Some(r) = rest.strip_prefix('#').or_else(|| rest.strip_prefix('b')) {
        rest = r;
    }
    // Qualities, then digits (an octave or an extension), possibly negative octave.
    loop {
        let before = rest.len();
        for q in ["maj", "min", "dim", "aug", "sus", "add", "m"] {
            if let Some(r) = rest.strip_prefix(q) {
                rest = r;
                break;
            }
        }
        if rest.len() == before {
            break;
        }
    }
    let rest = rest.strip_prefix('-').unwrap_or(rest);
    rest.len() <= 2 && rest.bytes().all(|c| c.is_ascii_digit())
}

/// Takes a tempo (`124 BPM`, `124bpm`) out of `seg`.
fn take_bpm(seg: &str) -> Option<(u16, String)> {
    let words: Vec<&str> = seg.split(|c: char| c.is_whitespace() || c == '_').filter(|w| !w.is_empty()).collect();
    for (i, w) in words.iter().enumerate() {
        let lw = w.to_lowercase();
        let (num, span) = if let Some(n) = lw.strip_suffix("bpm").filter(|n| !n.is_empty()) {
            (n.to_string(), 1)
        } else if words.get(i + 1).is_some_and(|n| n.eq_ignore_ascii_case("bpm")) {
            (lw.clone(), 2)
        } else {
            continue;
        };
        let Ok(bpm) = num.parse::<u16>() else { continue };
        if !(40..=300).contains(&bpm) {
            continue;
        }
        let rest: Vec<&str> = words.iter().enumerate().filter(|(j, _)| *j < i || *j >= i + span).map(|(_, w)| *w).collect();
        return Some((bpm, rest.join(" ")));
    }
    None
}

/// The name of a sound shown under `folders` (top first): the words
/// repeating its folder dropped, a tempo or a key moved to badges.
/// Never empty, never just a number.
pub fn label(name: &str, folders: &[&str]) -> Label {
    let mut segs: Vec<String> = name.split(" - ").map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    let mut out = Label::default();
    for seg in segs.iter_mut() {
        if let Some((bpm, rest)) = take_bpm(seg) {
            out.bpm = Some(bpm);
            *seg = rest;
            break;
        }
    }
    segs.retain(|s| !s.is_empty());
    if segs.len() >= 2 && segs.last().is_some_and(|s| is_key_name(s)) {
        out.key = segs.pop();
    }
    let meaningful = |s: &str| s.chars().any(char::is_alphabetic);
    if segs.len() >= 2 && folders.iter().any(|f| names_folder(&segs[0], f)) && segs[1..].iter().any(|s| meaningful(s)) {
        segs.remove(0);
    } else if let Some((kind, word)) = segs.first().filter(|_| segs.len() >= 2).and_then(|s| s.rsplit_once(' '))
        && folders.last().is_some_and(|f| names_folder(word, f))
    {
        // `Closed Hat - Tight 01` in Hats: `Closed · Tight 01`.
        let rest = segs[1..].join(" - ");
        segs = vec![format!("{} · {rest}", kind.trim())];
    } else if let (Some(first), Some(folder)) = (segs.first(), folders.last()) {
        // `House Beat 01` in Loops/House: the leading word goes.
        if let Some((word, rest)) = first.split_once(' ')
            && names_folder(word, folder)
            && meaningful(rest)
        {
            segs[0] = rest.trim().to_string();
        }
    }
    out.title = if segs.is_empty() { name.trim().to_string() } else { segs.join(" - ") };
    out
}

// --- Looks ----------------------------------------------------------------------

/// Icon and color slot (in the theme's pattern colors) of a top folder,
/// guessed from its name; `fallback` for names it does not know.
pub fn folder_look(name: &str, fallback: Icon) -> (Icon, usize) {
    let n = name.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| n.contains(w));
    if n == USER_ROOT.to_lowercase() {
        (Icon::Folder, 7)
    } else if has(&["drum", "perc", "rhythm"]) {
        (Icon::Drum, 0)
    } else if has(&["loop", "break"]) {
        (Icon::Loop, 1)
    } else if has(&["bass", "808"]) {
        (Icon::Bass, 2)
    } else if has(&["key", "piano", "acoustic"]) {
        (Icon::Piano, 4)
    } else if has(&["synth", "pad", "lead", "pluck"]) {
        (Icon::Instrument, 3)
    } else if has(&["vocal", "vox", "voice"]) {
        (Icon::Mic, 6)
    } else if has(&["fx", "sweep", "riser"]) {
        (Icon::Wand, 5)
    } else if has(&["sampler"]) {
        (Icon::Sample, 6)
    } else if has(&["dynamic", "mixing"]) {
        (Icon::Normalize, 1)
    } else if has(&["eq", "filter"]) {
        (Icon::Spectral, 3)
    } else if has(&["space", "reverb", "delay"]) {
        (Icon::Effect, 6)
    } else if has(&["modulation", "pitch"]) {
        (Icon::Automation, 5)
    } else if has(&["saturation", "distortion"]) {
        (Icon::Effect, 2)
    } else if has(&["utilit", "external"]) {
        (Icon::Cpu, 7)
    } else {
        (fallback, 0)
    }
}

/// Shape of the little waveform drawn before a sound (seven bar heights,
/// 0 to 1), from what the sound is: a kick hits and dies, a pad swells,
/// a loop repeats, a riser climbs.
pub fn glyph(tags: TagSet) -> [f32; 7] {
    let is = |t: Tag| tags.has(t);
    if is(Tag::Loop) {
        [0.95, 0.35, 0.6, 0.3, 0.9, 0.4, 0.55]
    } else if is(Tag::Kick) {
        [1.0, 0.8, 0.5, 0.3, 0.18, 0.1, 0.05]
    } else if is(Tag::Snare) || is(Tag::Clap) {
        [0.85, 1.0, 0.55, 0.35, 0.2, 0.12, 0.06]
    } else if is(Tag::Hat) || is(Tag::Perc) {
        [0.7, 0.35, 0.15, 0.06, 0.0, 0.0, 0.0]
    } else if is(Tag::Cymbal) {
        [0.9, 0.8, 0.7, 0.55, 0.45, 0.3, 0.2]
    } else if is(Tag::Fx) {
        [0.1, 0.2, 0.3, 0.45, 0.6, 0.8, 1.0]
    } else if is(Tag::Pad) {
        [0.2, 0.5, 0.75, 0.85, 0.85, 0.75, 0.55]
    } else if is(Tag::Bass) {
        [0.9, 1.0, 0.85, 0.75, 0.6, 0.45, 0.3]
    } else if is(Tag::Synth) || is(Tag::Keys) || is(Tag::Vocal) {
        [0.85, 0.6, 0.7, 0.5, 0.4, 0.3, 0.2]
    } else {
        [0.4, 0.75, 1.0, 0.65, 0.85, 0.45, 0.3]
    }
}

// --- Drawing --------------------------------------------------------------------

/// Key prefix of each browser tab (instruments, effects, sounds, plugins).
pub const TAB_PREFIX: [&str; 4] = ["i", "e", "s", "p"];

/// Indentation of a tree level.
const INDENT: f32 = 12.0;

/// Name shown for a folder: library folders as found on disk, categories
/// translated.
pub fn folder_label(tab: usize, name: &str) -> String {
    if name == USER_ROOT {
        tr("Your samples").to_string()
    } else if tab == 2 {
        name.to_string()
    } else {
        tr(name).to_string()
    }
}

fn tab_icon(tab: usize) -> Icon {
    match tab {
        0 => Icon::Instrument,
        1 => Icon::Effect,
        2 => Icon::Folder,
        _ => Icon::Plugin,
    }
}

/// What starts a row: a little waveform for a sound, an icon otherwise.
#[derive(Clone, Copy)]
pub enum Lead {
    Glyph([f32; 7]),
    Icon(Icon),
}

/// What an item row shows.
pub struct ItemView<'a> {
    pub lead: Lead,
    pub tint: Color,
    pub title: &'a str,
    /// Second line, muted (its folder in the search results).
    pub sub: Option<&'a str>,
    /// Small pills on the right (text, tint): tempo, key, plugin format.
    pub badges: &'a [(String, Color)],
    pub tip: &'a str,
    pub preview: bool,
    pub favorite: bool,
    pub playing: bool,
    pub selected: bool,
    /// 0 to 1 while its folder opens or closes.
    pub reveal: f32,
}

/// What a folder row shows.
struct FolderView {
    depth: usize,
    name: String,
    /// What a top folder holds, under its name while it is closed.
    sub: Option<String>,
    count: usize,
    icon: Icon,
    tint: Color,
    /// 0 (closed) to 1 (open), animated.
    open: f32,
    selected: bool,
    reveal: f32,
}

/// The tree drawn: where it ends, the picked row (to keep in sight after a
/// key), and the item under the pointer (to preview).
pub struct TreeOut {
    pub y: f32,
    pub sel_row: Option<Rect>,
    pub moved: bool,
    pub hovered_key: Option<String>,
}

impl App {
    /// Lead, title and badges of an entry shown inside `folders` (top first).
    pub(super) fn entry_look(&self, entry: &BrowserEntry, folders: &[&str]) -> (Lead, Color, String, Vec<(String, Color)>) {
        let t = self.theme;
        let top = folders.first().copied().unwrap_or("");
        let tint = t.patterns[folder_look(top, tab_icon(entry.cat)).1 % t.patterns.len()];
        match &entry.payload {
            Payload::Sample(_) => {
                let l = label(&entry.name, folders);
                let mut badges = Vec::new();
                if let Some(bpm) = l.bpm {
                    badges.push((tr_fmt!("{} BPM", bpm), t.accent));
                }
                if let Some(key) = l.key {
                    badges.push((key, t.select));
                }
                (Lead::Glyph(glyph(entry.tags)), tint, l.title, badges)
            }
            Payload::Instrument { kind, .. } | Payload::Effect { kind, .. } => {
                // One color per format, from the theme.
                let badges = match plugin_badge(kind) {
                    Some(b) => {
                        let col = match b {
                            "CLAP" => t.accent,
                            "VST3" => t.select,
                            "LV2" => t.warning,
                            _ => t.frost,
                        };
                        vec![(b.to_string(), col)]
                    }
                    None => Vec::new(),
                };
                let icon = match (entry.cat, &entry.payload) {
                    (3, _) => Icon::Plugin,
                    (_, Payload::Instrument { .. }) => Icon::Instrument,
                    _ => Icon::Effect,
                };
                (Lead::Icon(icon), tint, entry.name.clone(), badges)
            }
            Payload::Files(_) => (Lead::Icon(Icon::Folder), tint, entry.name.clone(), Vec::new()),
        }
    }

    /// The tree of the tab shown, from `y`: folders that open and close with
    /// a click or the arrows, their items below them.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn browser_tree(
        &mut self,
        c: &mut Canvas,
        input: &Input,
        list: Rect,
        x: f32,
        w: f32,
        y0: f32,
        all: &[BrowserEntry],
        tree: &Tree,
        keys: &[NavKey],
    ) -> TreeOut {
        let t = self.theme;
        let tab = self.sidebar_tab;
        let logical_rows = |prefs: &BrowserPrefs| tree.rows(&|f| prefs.is_open(&tree.folders[f].key) as u8 as f32);
        let mut moved = false;
        // A folder picked among the search results: opened, with its parents.
        if let Some(key) = self.browser.reveal.take()
            && let Some(f) = tree.find(&key)
        {
            for g in tree.lineage(f) {
                self.browser.prefs.set_open(&tree.folders[g].key, true);
            }
            self.save_browser_prefs();
            self.browser.sel = logical_rows(&self.browser.prefs).iter().position(|r| r.node == Node::Folder(f));
            moved = true;
        }
        let mut logical = logical_rows(&self.browser.prefs);
        for &key in keys {
            let prefs = &self.browser.prefs;
            let n = nav(&logical, self.browser.sel, key, &|f| prefs.is_open(&tree.folders[f].key));
            if let Some((f, open)) = n.fold {
                self.browser.prefs.set_open(&tree.folders[f].key, open);
                self.save_browser_prefs();
                logical = logical_rows(&self.browser.prefs);
                moved = true;
            }
            if n.sel != self.browser.sel {
                self.browser.sel = n.sel;
                moved = true;
                if let Some(Node::Item(e)) = n.sel.and_then(|s| logical.get(s)).map(|r| r.node)
                    && super::browser::previewable(&all[e].payload)
                {
                    let cmd = self.browser.preview.select(&item_key(&all[e].payload), self.browser.prefs.auto_preview);
                    self.run_preview(cmd);
                }
            }
            if let Some(e) = n.activate {
                self.add_default(all[e].payload.clone());
            }
        }
        let sel_node = self.browser.sel.and_then(|s| logical.get(s)).map(|r| r.node);

        // Each folder opens and closes smoothly; its rows unfold with it.
        let open_v: Vec<f32> = tree
            .folders
            .iter()
            .map(|f| {
                let target = self.browser.prefs.is_open(&f.key) as u8 as f32;
                self.anim.to(wid(&f.key, 0x6f70, 0), target, T_LAYOUT * 1.4)
            })
            .collect();
        let rows = tree.rows(&|f| open_v[f]);

        let hot = self.browser.hot_folder;
        let mut new_hot = None;
        let mut out = TreeOut { y: y0, sel_row: None, moved, hovered_key: None };
        let mut toggle = None;
        let mut picked = None;
        let mut y = y0;
        for (i, row) in rows.iter().enumerate() {
            let top_folder = matches!(row.node, Node::Folder(f) if tree.folders[f].depth == 0);
            let h = t.dense(if top_folder { 44.0 } else { 32.0 });
            let a = row.reveal;
            let slot = h * a;
            if slot < 0.5 {
                continue;
            }
            if top_folder && i > 0 {
                y += 4.0;
            }
            let indent = row.depth as f32 * INDENT;
            let r = Rect::new(x + indent, y, w - indent, h);
            if sel_node == Some(row.node) {
                out.sel_row = Some(Rect::new(x, y, w, h));
            }
            if y + slot > list.y && y < list.bottom() {
                let clip = (a < 0.999).then(|| c.push_clip(Rect::new(list.x, y, list.w, slot)));
                // Indentation guides: one line per level, the hovered folder's lit.
                for k in 0..row.depth {
                    let lit = k + 1 == row.depth && row.parent.is_some() && row.parent == hot;
                    let col = if lit { t.accent.alpha(0.5 * a) } else { t.hairline.alpha(a) };
                    c.fill_rect(Rect::new(x + k as f32 * INDENT + 12.0, y, 1.0, slot + 2.0), col);
                }
                match row.node {
                    Node::Folder(f) => {
                        let fo = &tree.folders[f];
                        let (icon, ci) = folder_look(&tree.folders[fo.top].name, tab_icon(tab));
                        let tint = t.patterns[ci % t.patterns.len()];
                        // A top folder says what it holds, until it opens.
                        let sub = (fo.depth == 0).then(|| {
                            let names: Vec<String> = if fo.folders.is_empty() {
                                fo.items.iter().map(|&e| all[e].name.clone()).collect()
                            } else {
                                fo.folders.iter().map(|&g| folder_label(tab, &tree.folders[g].name)).collect()
                            };
                            names.join(", ")
                        });
                        let view = FolderView {
                            depth: fo.depth,
                            name: folder_label(tab, &fo.name),
                            sub,
                            count: fo.count,
                            icon,
                            tint,
                            open: open_v[f],
                            selected: sel_node == Some(row.node),
                            reveal: a,
                        };
                        let (clicked, hov) = self.folder_row(c, input, wid(&fo.key, 0x666f, 0), r, &view);
                        if hov {
                            new_hot = Some(f);
                        }
                        if clicked {
                            toggle = Some(f);
                        }
                    }
                    Node::Item(e) => {
                        let entry = &all[e];
                        let names: Vec<&str> =
                            row.parent.map_or(Vec::new(), |p| tree.lineage(p).iter().map(|&g| tree.folders[g].name.as_str()).collect());
                        let (lead, tint, title, badges) = self.entry_look(entry, &names);
                        let item = item_key(&entry.payload);
                        let tip = if entry.cat == 2 {
                            tr_fmt!("{}\nClick: add · drag: drop", entry.name)
                        } else {
                            tr_fmt!("{}\nClick: add · drag: drop", entry.detail)
                        };
                        let view = ItemView {
                            lead,
                            tint,
                            title: &title,
                            sub: None,
                            badges: &badges,
                            tip: &tip,
                            preview: super::browser::previewable(&entry.payload),
                            favorite: self.browser.prefs.is_favorite(&item),
                            playing: self.browser.preview.playing.as_deref() == Some(item.as_str()),
                            selected: sel_node == Some(row.node),
                            reveal: a,
                        };
                        let (hit, hov) = self.item_row(c, input, wid(&item, 0x6974, 0), r, &view);
                        if hov {
                            new_hot = row.parent;
                            if view.preview {
                                out.hovered_key = Some(item.clone());
                            }
                        }
                        match hit {
                            RowHit::None => {}
                            RowHit::Grab => {
                                picked = Some(row.node);
                                self.stop_preview();
                                self.begin_dnd(input, entry.payload.clone());
                            }
                            RowHit::Play => {
                                picked = Some(row.node);
                                let cmd = self.browser.preview.click_play(&item);
                                self.run_preview(Some(cmd));
                            }
                            RowHit::Star => self.toggle_favorite(&item),
                        }
                    }
                }
                if let Some(prev) = clip {
                    c.set_clip(Some(prev));
                }
            }
            y += slot + 2.0 * a;
        }
        self.browser.hot_folder = new_hot;
        if let Some(f) = toggle {
            let open = !self.browser.prefs.is_open(&tree.folders[f].key);
            self.browser.prefs.set_open(&tree.folders[f].key, open);
            self.save_browser_prefs();
            picked = Some(Node::Folder(f));
        }
        // A click picks the row: the arrows go on from there.
        if let Some(node) = picked {
            self.browser.sel = logical_rows(&self.browser.prefs).iter().position(|r| r.node == node);
        }
        out.y = y;
        out
    }

    /// A folder row: the chevron turning as it opens, its icon (a colored
    /// tile at the top level), its name and how many items it holds.
    /// Returns whether it was clicked, and whether it is hovered.
    fn folder_row(&mut self, c: &mut Canvas, input: &Input, id: u64, r: Rect, v: &FolderView) -> (bool, bool) {
        let (depth, open, a, tint) = (v.depth, v.open, v.reveal, v.tint);
        let (name, count, selected) = (v.name.as_str(), v.count, v.selected);
        let t = self.theme;
        let hov = self.hovered(input, id, r) && !self.dnd_active() && a > 0.5;
        let hv = self.hover_anim(id, hov);
        let cy = r.y + r.h * 0.5;
        if hv > 0.01 {
            c.fill_rrect(r, 10.0, t.raised.alpha(hv * a));
        }
        if selected {
            c.stroke_rrect(r, 10.0, 1.0, t.select.alpha(0.8 * a));
        }
        // Chevron: points right when closed, down when open.
        let (sin, cos) = (open * std::f32::consts::FRAC_PI_2).sin_cos();
        let (chx, s) = (r.x + 12.0, if depth == 0 { 1.0 } else { 0.85 });
        let pt = |px: f32, py: f32| (chx + (px * cos - py * sin) * s, cy + (px * sin + py * cos) * s);
        let chev = t.muted.mix(t.text, hv.max(open * 0.5)).alpha(a);
        c.stroke_polyline(&[pt(-2.0, -4.5), pt(2.5, 0.0), pt(-2.0, 4.5)], 1.6, chev);
        let name_col = t.text2.mix(t.text, hv.max(open)).alpha(a);
        let name_x = if depth == 0 {
            let tile = Rect::new(r.x + 26.0, cy - 15.0, 30.0, 30.0);
            c.fill_rrect(tile, 9.0, tint.alpha((0.13 + 0.07 * hv + 0.06 * open) * a));
            draw_icon(c, v.icon, tile.x + 15.0, cy, 17.0, tint.alpha(a));
            tile.right() + 10.0
        } else {
            draw_icon(c, Icon::Folder, r.x + 30.0, cy, 13.0, tint.alpha((0.65 + 0.35 * hv.max(open)) * a));
            r.x + 44.0
        };
        // The count, in a pill at the top level.
        let count_s = count.to_string();
        let cw = self.fonts.measure(self.f.mono_med, 10.5, &count_s);
        let right = r.right() - 8.0;
        if depth == 0 {
            let pill = Rect::new(right - cw - 14.0, cy - 10.0, cw + 14.0, 20.0);
            c.fill_rrect(pill, 10.0, t.raised_hi.alpha((0.55 + 0.45 * hv) * a));
            self.text(c, self.f.mono_med, 10.5, pill.x + 7.0, cy, &count_s, t.text2.mix(t.text, hv).alpha(a));
        } else {
            self.text(c, self.f.mono_med, 10.5, right - cw - 4.0, cy, &count_s, t.muted.mix(t.text2, hv).alpha(a));
        }
        let size = if depth == 0 { 14.0 } else { 13.0 };
        let name_w = right - cw - 22.0 - name_x;
        match &v.sub {
            // The name rises to make room for what it holds, and settles back as it opens.
            Some(sub) if open < 0.999 => {
                let k = 1.0 - open;
                self.text_fit(c, self.f.sans_med, size, name_x, cy - 7.5 * k, name, name_w, name_col);
                self.text_fit(c, self.f.sans, 11.0, name_x, cy + 9.0 * k + 2.0 * open, sub, name_w, t.muted.alpha(a * k * k));
            }
            _ => self.text_fit(c, self.f.sans_med, size, name_x, cy, name, name_w, name_col),
        }
        if !hov {
            return (false, false);
        }
        self.set_cursor(ponzik_window::Cursor::Hand);
        self.set_tip(&tr_fmt!("{}: {} items\nClick or arrows: open, close", name, count), "");
        (input.pressed, true)
    }

    /// An item row: its lead (a waveform glyph or an icon, a play button
    /// under the pointer), its name, its badges, and on hover the star and
    /// the plus. Returns what a click touched, and whether it is hovered.
    pub(super) fn item_row(&mut self, c: &mut Canvas, input: &Input, id: u64, r: Rect, v: &ItemView) -> (RowHit, bool) {
        let t = self.theme;
        let a = v.reveal;
        let hov = self.hovered(input, id, r) && !self.dnd_active() && a > 0.5;
        let hv = self.hover_anim(id, hov);
        let pv = self.anim.to(id ^ 0x504c, v.playing as u8 as f32, T_HOVER);
        if hv > 0.01 || pv > 0.01 {
            c.fill_rrect(r, 8.0, t.raised.alpha(hv.max(pv) * a).mix(t.accent.alpha(0.12 * a), pv));
        }
        if v.selected {
            c.stroke_rrect(r, 8.0, 1.0, t.select.alpha(0.8 * a));
        }
        let cy = r.y + r.h * 0.5;
        let lead = Rect::new(r.x + 5.0, cy - 13.0, 26.0, 26.0);
        let (lx, ly) = lead.center();
        let phov = hov && v.preview && lead.contains(input.x, input.y);
        // Lead: the glyph (or icon) gives way to a play button under the pointer.
        let show_play = if v.preview && !v.playing { hv } else { 0.0 };
        if v.playing {
            for i in 0..5 {
                let k = 0.5 + 0.5 * (self.time * 9.0 + i as f32 * 1.7).sin();
                let h = 4.0 + 10.0 * k;
                c.fill_rrect(Rect::new(lx - 8.0 + i as f32 * 3.5, ly - h * 0.5, 2.2, h), 1.0, t.accent.alpha(a));
            }
            self.anim.set(wid("br-eq", 0, 0), self.time);
        } else if show_play < 0.99 {
            let k = 1.0 - show_play;
            match v.lead {
                Lead::Glyph(bars) => {
                    for (i, b) in bars.iter().enumerate() {
                        let h = (2.0 + 13.0 * b) * (0.6 + 0.4 * k);
                        c.fill_rrect(Rect::new(lx - 9.5 + i as f32 * 3.1, ly - h * 0.5, 1.8, h), 0.9, v.tint.alpha(0.75 * a * k));
                    }
                }
                Lead::Icon(icon) => draw_icon(c, icon, lx, ly, 15.0, v.tint.alpha(0.9 * a * k)),
            }
        }
        if show_play > 0.01 {
            let ring = if phov { t.accent } else { v.tint };
            c.fill_circle(lx, ly, 11.0 * (0.7 + 0.3 * show_play), ring.alpha((if phov { 0.3 } else { 0.16 }) * show_play * a));
            draw_icon(c, Icon::Play, lx + 1.0, ly, 11.0, t.text.mix(t.accent, if phov { 1.0 } else { 0.0 }).alpha(show_play * a));
        }
        // Right side: star and plus on hover (the star stays on a favorite), badges before them.
        let star_cx = r.right() - 14.0 - 24.0 * hv;
        let mut right = r.right() - 6.0 - if v.favorite { 22.0 + 24.0 * hv } else { 46.0 * hv };
        for (text, tint) in v.badges.iter().rev() {
            let bw = self.fonts.measure(self.f.mono_med, 9.5, text) + 12.0;
            let pill = Rect::new(right - bw, cy - 9.0, bw, 18.0);
            c.fill_rrect(pill, 9.0, tint.alpha((0.12 + 0.08 * hv) * a));
            let (px, py) = pill.center();
            self.text_center(c, self.f.mono_med, 9.5, px, py, text, tint.alpha(a));
            right = pill.x - 4.0;
        }
        let title_x = r.x + 37.0;
        let title_col = t.text2.mix(t.text, hv.max(pv)).alpha(a);
        match v.sub {
            Some(sub) => {
                self.text_fit(c, self.f.sans_med, 13.0, title_x, cy - 7.5, v.title, right - 4.0 - title_x, title_col);
                self.text_fit(c, self.f.sans, 11.0, title_x, cy + 8.5, sub, right - 4.0 - title_x, t.muted.alpha(a));
            }
            None => self.text_fit(c, self.f.sans_med, 13.0, title_x, cy, v.title, right - 4.0 - title_x, title_col),
        }
        let star = Rect::new(star_cx - 12.0, cy - 12.0, 24.0, 24.0);
        let shov = hov && star.contains(input.x, input.y);
        if v.favorite {
            draw_icon(c, Icon::StarFilled, star_cx, cy, 13.0, if shov { t.text } else { t.accent }.alpha(a));
        } else if hv > 0.01 {
            draw_icon(c, Icon::Star, star_cx, cy, 13.0, t.muted.alpha(hv * a).mix(t.text, if shov { 1.0 } else { 0.0 }));
        }
        if hv > 0.01 {
            draw_icon(c, Icon::Plus, r.right() - 14.0, cy, 13.0, t.accent.alpha(hv * a));
        }
        if !hov {
            return (RowHit::None, false);
        }
        if phov || shov {
            self.set_cursor(ponzik_window::Cursor::Hand);
        }
        let tip = if phov {
            tr("Listen")
        } else if shov {
            if v.favorite { tr("Remove from the favorites") } else { tr("Add to the favorites") }
        } else {
            v.tip
        };
        self.set_tip(tip, "");
        let hit = match (input.pressed, phov, shov) {
            (false, ..) => RowHit::None,
            (true, true, _) => RowHit::Play,
            (true, _, true) => RowHit::Star,
            _ => RowHit::Grab,
        };
        (hit, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(p: &str) -> Vec<String> {
        p.split('/').filter(|s| !s.is_empty()).map(str::to_string).collect()
    }

    fn sample_tree() -> Tree {
        Tree::build(
            "s",
            [
                (0, path("Drums/Kicks")),
                (1, path("Drums/Kicks")),
                (2, path("Drums/Snares")),
                (3, path("Loops/House")),
                (4, path("Drums/Kicks")),
                (5, path("")),
                (6, path("Your samples/Downloads")),
            ],
        )
    }

    #[test]
    fn folders_group_items_in_first_seen_order() {
        let t = sample_tree();
        let names: Vec<&str> = t.roots.iter().map(|&f| t.folders[f].name.as_str()).collect();
        assert_eq!(names, ["Drums", "Loops", "Your samples"]);
        let drums = t.find("s/Drums").unwrap();
        assert_eq!(t.folders[drums].count, 4);
        let kicks = t.find("s/Drums/Kicks").unwrap();
        assert_eq!(t.folders[kicks].items, [0, 1, 4]);
        assert_eq!(t.folders[kicks].depth, 1);
        assert_eq!(t.folders[kicks].top, drums);
        assert_eq!(t.loose, [5]);
        assert_eq!(t.lineage(kicks), [drums, kicks]);
        let house = t.find("s/Loops/House").unwrap();
        assert_eq!(t.folders[house].items, [3]);
        // Another tab's prefix: other keys.
        assert!(Tree::build("i", [(0, path("Drums"))]).find("s/Drums").is_none());
    }

    #[test]
    fn rows_follow_the_open_folders() {
        let t = sample_tree();
        let closed = t.rows(&|_| 0.0);
        assert_eq!(closed.len(), 4, "three top folders and the loose item");
        let drums = t.find("s/Drums").unwrap();
        let kicks = t.find("s/Drums/Kicks").unwrap();
        let open = t.rows(&|f| (f == drums || f == kicks) as u8 as f32);
        let nodes: Vec<Node> = open.iter().map(|r| r.node).collect();
        let snares = t.find("s/Drums/Snares").unwrap();
        assert_eq!(nodes[..6], [Node::Folder(drums), Node::Folder(kicks), Node::Item(0), Node::Item(1), Node::Item(4), Node::Folder(snares)]);
        assert_eq!(open[2].depth, 2);
        assert_eq!(open[2].parent, Some(kicks));
        // Half open: the rows inside are half revealed.
        let half = t.rows(&|f| if f == drums { 0.5 } else { 1.0 });
        assert!(half.iter().filter(|r| r.parent == Some(kicks)).all(|r| (r.reveal - 0.5).abs() < 1e-6));
    }

    #[test]
    fn arrows_walk_open_and_close() {
        let t = sample_tree();
        let drums = t.find("s/Drums").unwrap();
        let kicks = t.find("s/Drums/Kicks").unwrap();
        let open = |f: usize| f == drums || f == kicks;
        let rows = t.rows(&|f| open(f) as u8 as f32);
        // Nothing picked: Down picks the first row, Up the last.
        assert_eq!(nav(&rows, None, NavKey::Down, &open).sel, Some(0));
        assert_eq!(nav(&rows, None, NavKey::Up, &open).sel, Some(rows.len() - 1));
        // Right on an open folder enters it; Left on it closes it.
        assert_eq!(nav(&rows, Some(0), NavKey::Right, &open).sel, Some(1));
        assert_eq!(nav(&rows, Some(0), NavKey::Left, &open).fold, Some((drums, false)));
        // Left on an item goes to its folder.
        assert_eq!(nav(&rows, Some(3), NavKey::Left, &open).sel, Some(1));
        // Right on a closed folder opens it.
        let snares = t.find("s/Drums/Snares").unwrap();
        let at = rows.iter().position(|r| r.node == Node::Folder(snares)).unwrap();
        assert_eq!(nav(&rows, Some(at), NavKey::Right, &open).fold, Some((snares, true)));
        // Enter toggles a folder, adds an item.
        assert_eq!(nav(&rows, Some(1), NavKey::Enter, &open).fold, Some((kicks, false)));
        assert_eq!(nav(&rows, Some(2), NavKey::Enter, &open).activate, Some(0));
        // The ends hold.
        assert_eq!(nav(&rows, Some(0), NavKey::Up, &open).sel, Some(0));
        let last = rows.len() - 1;
        assert_eq!(nav(&rows, Some(last), NavKey::Down, &open).sel, Some(last));
    }

    #[test]
    fn names_drop_their_folder_and_keep_badges() {
        let l = |n: &str, f: &str| label(n, &f.split('/').collect::<Vec<_>>());
        assert_eq!(l("Kick - Punchy House 01", "Drums/Kicks").title, "Punchy House 01");
        assert_eq!(l("Perc - Conga High 01", "Drums/Percussion").title, "Conga High 01");
        // Not the folder's word: kept.
        assert_eq!(l("Crash - Bright 01", "Drums/Cymbals").title, "Crash - Bright 01");
        assert_eq!(l("Closed Hat - Tight 01", "Drums/Hats").title, "Closed · Tight 01");
        // Loops: the genre word goes, the tempo becomes a badge.
        let house = l("House Beat 01 - 124 BPM", "Loops/House");
        assert_eq!((house.title.as_str(), house.bpm), ("Beat 01", Some(124)));
        assert_eq!(l("Hip-Hop Boom Bap 01 - 90 BPM", "Loops/Hip-Hop").title, "Boom Bap 01");
        assert_eq!(l("break_174bpm", "Your samples/Downloads").bpm, Some(174));
        // A key or a note at the end becomes a badge.
        let b = l("808 - Clean - C#1", "Bass/808");
        assert_eq!((b.title.as_str(), b.key.as_deref()), ("Clean", Some("C#1")));
        let b = l("Bass - Reese - C1", "Bass/Synth");
        assert_eq!((b.title.as_str(), b.key.as_deref()), ("Reese", Some("C1")));
        let p = l("Pad - Warm Saw - Cmaj7", "Synths/Pads");
        assert_eq!((p.title.as_str(), p.key.as_deref()), ("Warm Saw", Some("Cmaj7")));
        assert_eq!(l("Riser - Snare Roll 124 BPM", "FX/Risers").title, "Snare Roll");
        // Never down to a bare number or nothing.
        assert_eq!(l("Kick - 01", "Drums/Kicks").title, "Kick - 01");
        assert_eq!(l("Kick 01", "Drums/Kicks").title, "Kick 01");
        assert_eq!(l("Kicks", "Drums/Kicks").title, "Kicks");
        assert_eq!(l("My take", "").title, "My take");
    }

    #[test]
    fn key_names() {
        for k in ["C", "F#1", "Eb", "Cm7", "Fmaj9", "Am", "Ebmaj9", "Dsus4", "C-1", "Dm9"] {
            assert!(is_key_name(k), "{k}");
        }
        for k in ["Dark", "Bass", "Big", "Glass FM", "Square", "Ab123", "", "c1"] {
            assert!(!is_key_name(k), "{k}");
        }
    }
}
