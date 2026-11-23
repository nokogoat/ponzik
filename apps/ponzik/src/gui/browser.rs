//! Browser library: favorites, recent items, tags inferred from the sample
//! names, and the sound preview (hover a row or pick it with the arrows: it
//! plays through the engine's audition voice, outside the mixer).
//!
//! Loops are previewed in time while the song plays: stretched to the project
//! tempo from their detected tempo, starting on the next beat. One-shots play
//! at once. A native instrument plays a short chord, rendered off the
//! interface thread.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};

use ponzik_engine::Sample;
use ponzik_project::json::{self, Json};

use super::dnd::Payload;

// --- Tags -----------------------------------------------------------------------

/// What a sample is, guessed from its folders and file name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tag {
    Kick,
    Snare,
    Clap,
    Hat,
    Cymbal,
    Perc,
    Loop,
    Fx,
    Bass,
    Synth,
    Pad,
    Keys,
    Vocal,
}

impl Tag {
    pub const ALL: [Tag; 13] = [
        Tag::Kick,
        Tag::Snare,
        Tag::Clap,
        Tag::Hat,
        Tag::Cymbal,
        Tag::Perc,
        Tag::Loop,
        Tag::Fx,
        Tag::Bass,
        Tag::Synth,
        Tag::Pad,
        Tag::Keys,
        Tag::Vocal,
    ];

    fn bit(self) -> u16 {
        1 << Tag::ALL.iter().position(|&t| t == self).unwrap_or(0)
    }

    /// Words that give the tag away. Short ones must match a whole word,
    /// longer ones may start a word (`hihat`, `kicks`).
    fn words(self) -> &'static [&'static str] {
        match self {
            Tag::Kick => &["kick", "kik", "kck", "bd", "bassdrum"],
            Tag::Snare => &["snare", "snr", "sd", "rim", "rims", "rimshot"],
            Tag::Clap => &["clap", "clp", "snap", "snaps"],
            Tag::Hat => &["hat", "hats", "hihat", "hh", "openhat", "closedhat"],
            Tag::Cymbal => &["cymbal", "crash", "ride", "splash", "china"],
            Tag::Perc => &["perc", "percussion", "tom", "toms", "conga", "bongo", "shaker", "shakers", "tamb", "tambourine", "cowbell", "clave", "woodblock"],
            Tag::Loop => &["loop", "loops", "break", "breaks", "breakbeat", "groove", "bpm"],
            Tag::Fx => &["fx", "sfx", "riser", "risers", "sweep", "sweeps", "impact", "impacts", "uplifter", "downlifter", "whoosh", "transition", "noise"],
            Tag::Bass => &["bass", "sub", "808", "reese"],
            Tag::Synth => &["synth", "synths", "lead", "pluck", "plucks", "stab", "stabs", "arp"],
            Tag::Pad => &["pad", "pads", "atmos", "atmosphere", "drone", "texture", "ambient"],
            Tag::Keys => &["keys", "key", "piano", "rhodes", "organ", "epiano", "bell"],
            Tag::Vocal => &["vocal", "vocals", "vox", "voice", "acapella", "adlib", "chant"],
        }
    }
}

/// A set of tags, one bit each.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TagSet(u16);

impl TagSet {
    pub fn has(self, tag: Tag) -> bool {
        self.0 & tag.bit() != 0
    }

    pub fn insert(&mut self, tag: Tag) {
        self.0 |= tag.bit();
    }

    pub fn remove(&mut self, tag: Tag) {
        self.0 &= !tag.bit();
    }

    #[cfg(test)]
    pub fn iter(self) -> impl Iterator<Item = Tag> {
        Tag::ALL.into_iter().filter(move |&t| self.has(t))
    }
}

/// Lower-case words of a name: split on anything that is not a letter or a
/// digit, and between a lower-case letter and an upper-case one (`OpenHat`).
fn words(name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut prev_lower = false;
    for ch in name.chars() {
        if !ch.is_alphanumeric() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            prev_lower = false;
            continue;
        }
        if ch.is_uppercase() && prev_lower && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        prev_lower = ch.is_lowercase() || ch.is_ascii_digit();
        cur.extend(ch.to_lowercase());
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn word_matches(word: &str, key: &str) -> bool {
    word == key || (key.len() >= 4 && word.starts_with(key))
}

/// Tags of a sample, from its file name and its last two folders
/// (`Drums/Kicks/Kick 01.wav` → Kick; `Loops/House/… 124 BPM.wav` → Loop).
pub fn infer_tags(path: &Path) -> TagSet {
    let mut names: Vec<String> = Vec::new();
    if let Some(stem) = path.file_stem() {
        names.push(stem.to_string_lossy().into_owned());
    }
    let mut dir = path.parent();
    for _ in 0..2 {
        let Some(d) = dir else { break };
        if let Some(n) = d.file_name() {
            names.push(n.to_string_lossy().into_owned());
        }
        dir = d.parent();
    }
    let mut set = TagSet::default();
    for name in &names {
        let ws = words(name);
        for tag in Tag::ALL {
            if ws.iter().any(|w| tag.words().iter().any(|k| word_matches(w, k))) {
                set.insert(tag);
            }
            // "120bpm", "174BPM": a loop.
            if tag == Tag::Loop && ws.iter().any(|w| w.len() > 3 && w.ends_with("bpm") && w[..w.len() - 3].bytes().all(|b| b.is_ascii_digit())) {
                set.insert(tag);
            }
        }
    }
    // A bass synth is a bass; an 808 kick is a kick.
    // A pad or keys in a synth folder are told by the more precise name.
    if set.has(Tag::Bass) || set.has(Tag::Pad) || set.has(Tag::Keys) {
        set.remove(Tag::Synth);
    }
    if set.has(Tag::Kick) && !names.iter().any(|n| words(n).iter().any(|w| w == "bass")) {
        set.remove(Tag::Bass);
    }
    set
}

// --- Items, favorites and recent ------------------------------------------------

/// Stable name of a browser item, for the favorites and recent lists.
pub fn item_key(payload: &Payload) -> String {
    match payload {
        Payload::Instrument { kind, .. } => format!("instrument:{kind}"),
        Payload::Effect { kind, .. } => format!("effect:{kind}"),
        Payload::Sample(p) => format!("sample:{}", p.to_string_lossy()),
        Payload::Files(_) => String::new(),
    }
}

/// Most recent items kept.
pub const RECENT_MAX: usize = 30;

/// What the browser remembers between sessions (`browser.json`).
#[derive(Clone, Debug, PartialEq)]
pub struct BrowserPrefs {
    pub favorites: Vec<String>,
    /// Most recent first.
    pub recent: Vec<String>,
    pub auto_preview: bool,
    /// Preview volume, 0 to 1.
    pub volume: f32,
    /// Folders of the browser tree opened or closed by hand (key, open).
    pub folders: Vec<(String, bool)>,
}

impl Default for BrowserPrefs {
    fn default() -> Self {
        Self { favorites: Vec::new(), recent: Vec::new(), auto_preview: true, volume: 0.7, folders: Vec::new() }
    }
}

/// A folder never touched: the sounds' folders start closed (there are many),
/// the other tabs' open (a few devices each).
pub fn folder_open_by_default(key: &str) -> bool {
    !key.starts_with("s/")
}

impl BrowserPrefs {
    pub fn is_favorite(&self, key: &str) -> bool {
        self.favorites.iter().any(|k| k == key)
    }

    /// Stars or unstars `key`; returns its new state.
    pub fn toggle_favorite(&mut self, key: &str) -> bool {
        if let Some(i) = self.favorites.iter().position(|k| k == key) {
            self.favorites.remove(i);
            false
        } else {
            self.favorites.push(key.to_string());
            true
        }
    }

    /// `key` was just used: first of the recent list.
    pub fn push_recent(&mut self, key: &str) {
        if key.is_empty() {
            return;
        }
        self.recent.retain(|k| k != key);
        self.recent.insert(0, key.to_string());
        self.recent.truncate(RECENT_MAX);
    }

    /// Place of `key` in the recent list.
    pub fn recent_rank(&self, key: &str) -> Option<usize> {
        self.recent.iter().position(|k| k == key)
    }

    /// Whether the tree folder `key` is open.
    pub fn is_open(&self, key: &str) -> bool {
        self.folders.iter().find(|f| f.0 == key).map_or_else(|| folder_open_by_default(key), |f| f.1)
    }

    /// Opens or closes the folder `key`; a folder back to its default is forgotten.
    pub fn set_open(&mut self, key: &str, open: bool) {
        self.folders.retain(|f| f.0 != key);
        if open != folder_open_by_default(key) {
            self.folders.push((key.to_string(), open));
        }
    }

    /// Gain of the audition voice: the slider follows the ear (squared).
    pub fn gain(&self) -> f32 {
        self.volume * self.volume
    }

    pub fn to_json(&self) -> Json {
        let list = |v: &[String]| Json::Arr(v.iter().map(|s| Json::from(s.as_str())).collect());
        Json::Obj(vec![
            ("auto_preview".into(), self.auto_preview.into()),
            ("preview_volume".into(), self.volume.into()),
            ("favorites".into(), list(&self.favorites)),
            ("recent".into(), list(&self.recent)),
            ("folders".into(), Json::Obj(self.folders.iter().map(|(k, open)| (k.clone(), (*open).into())).collect())),
        ])
    }

    /// Missing or damaged fields keep their default.
    pub fn from_json(j: &Json) -> Self {
        let d = Self::default();
        let list = |name: &str| -> Vec<String> {
            let mut out: Vec<String> = Vec::new();
            for s in j.get(name).and_then(Json::as_arr).unwrap_or(&[]).iter().filter_map(Json::as_str) {
                if !s.is_empty() && !out.iter().any(|o| o == s) {
                    out.push(s.to_string());
                }
            }
            out
        };
        let mut recent = list("recent");
        recent.truncate(RECENT_MAX);
        let mut folders: Vec<(String, bool)> = Vec::new();
        for (k, v) in j.get("folders").and_then(Json::as_obj).unwrap_or(&[]) {
            if let Some(open) = v.as_bool()
                && !k.is_empty()
                && !folders.iter().any(|f| f.0 == *k)
            {
                folders.push((k.clone(), open));
            }
        }
        Self {
            favorites: list("favorites"),
            recent,
            folders,
            auto_preview: j.get("auto_preview").and_then(Json::as_bool).unwrap_or(d.auto_preview),
            volume: j.get("preview_volume").and_then(Json::as_f64).map_or(d.volume, |v| (v as f32).clamp(0.0, 1.0)),
        }
    }

    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path).ok().and_then(|t| json::parse(&t).ok()).map(|j| Self::from_json(&j)).unwrap_or_default()
    }

    /// Written through a temporary file: a crash never leaves half a file.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, self.to_json().pretty())?;
        std::fs::rename(&tmp, path)
    }
}

// --- Preview state --------------------------------------------------------------

/// How long the pointer rests on a row before it plays, in seconds: sweeping
/// over the list stays quiet.
pub const DWELL: f32 = 0.12;

/// What the preview must do now.
#[derive(Clone, Debug, PartialEq)]
pub enum PreviewCmd {
    Play(String),
    Stop,
}

/// The preview's state machine: which item is under the pointer, which one
/// plays. Pure: the caller turns its answers into engine commands.
#[derive(Clone, Debug, Default)]
pub struct Preview {
    /// Item under the pointer and since when.
    hovered: Option<(String, f32)>,
    /// The hovered item already played (or was stopped): not again until
    /// the pointer leaves it.
    fired: bool,
    /// Item playing (or being prepared to play).
    pub playing: Option<String>,
}

impl Preview {
    /// The pointer is on `key` (or on no previewable row) at `now`.
    pub fn hover(&mut self, key: Option<&str>, now: f32, auto: bool) -> Option<PreviewCmd> {
        let same = self.hovered.as_ref().map(|h| h.0.as_str()) == key;
        if !same {
            let left = self.hovered.is_some();
            self.hovered = key.map(|k| (k.to_string(), now));
            self.fired = false;
            // Leaving a row stops its sound (an arrow-picked one plays on).
            if left && self.playing.is_some() && self.playing.as_deref() != key {
                self.playing = None;
                return Some(PreviewCmd::Stop);
            }
            return None;
        }
        let (k, since) = self.hovered.as_ref()?;
        if auto && !self.fired && now - since >= DWELL {
            self.fired = true;
            if self.playing.as_deref() != Some(k) {
                self.playing = Some(k.clone());
                return Some(PreviewCmd::Play(k.clone()));
            }
        }
        None
    }

    /// Picked with the arrow keys: plays at once with auto-preview.
    pub fn select(&mut self, key: &str, auto: bool) -> Option<PreviewCmd> {
        if !auto {
            return None;
        }
        if self.hovered.as_ref().is_some_and(|h| h.0 == key) {
            self.fired = true;
        }
        self.playing = Some(key.to_string());
        Some(PreviewCmd::Play(key.to_string()))
    }

    /// The row's play button: starts it, or stops it if it is the one playing.
    pub fn click_play(&mut self, key: &str) -> PreviewCmd {
        if self.hovered.as_ref().is_some_and(|h| h.0 == key) {
            self.fired = true;
        }
        if self.playing.as_deref() == Some(key) {
            self.playing = None;
            PreviewCmd::Stop
        } else {
            self.playing = Some(key.to_string());
            PreviewCmd::Play(key.to_string())
        }
    }

    /// Auto-preview switched on or off: off stops what plays.
    pub fn set_auto(&mut self, auto: bool) -> Option<PreviewCmd> {
        // On: the row under the pointer plays after the dwell.
        self.fired = false;
        if let Some(h) = self.hovered.as_mut() {
            h.1 = f32::NEG_INFINITY;
        }
        (!auto && self.playing.take().is_some()).then_some(PreviewCmd::Stop)
    }

    /// Stops whatever plays (the browser closed, a drag began).
    pub fn stop(&mut self) -> Option<PreviewCmd> {
        self.hovered = None;
        self.playing.take().map(|_| PreviewCmd::Stop)
    }

    /// The voice finished on its own.
    pub fn finished(&mut self) {
        self.playing = None;
    }
}

// --- Preview audio --------------------------------------------------------------

/// Sound to prepare for a preview.
#[derive(Clone, Debug)]
pub enum Source {
    Sample(PathBuf),
    /// A native instrument: a chord if it plays notes, one hit otherwise.
    Instrument { kind: String, melodic: bool },
}

/// A sound ready to play.
pub struct Prepared {
    pub sample: Arc<Sample>,
    /// A loop (whole bars at its detected tempo): it plays looped, and starts
    /// on the next beat when it was stretched to the song.
    pub is_loop: bool,
    pub synced: bool,
}

struct Job {
    id: String,
    source: Source,
    /// Song tempo to stretch a loop to (the song plays), or none.
    tempo: Option<f32>,
}

/// Prepares the sounds in a background thread; a newer request replaces the
/// ones still waiting.
pub struct Preparer {
    tx: Sender<Job>,
    rx: Receiver<(String, Option<Prepared>)>,
}

impl Preparer {
    pub fn start() -> Option<Self> {
        let (tx, jobs) = mpsc::channel::<Job>();
        let (done, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("ponzik-preview".into())
            .spawn(move || {
                while let Ok(mut job) = jobs.recv() {
                    // Only the last request matters.
                    while let Ok(newer) = jobs.try_recv() {
                        job = newer;
                    }
                    let out = prepare(&job.source, job.tempo);
                    if done.send((job.id, out)).is_err() {
                        return;
                    }
                }
            })
            .ok()?;
        Some(Self { tx, rx })
    }

    pub fn request(&self, id: String, source: Source, tempo: Option<f32>) {
        let _ = self.tx.send(Job { id, source, tempo });
    }

    pub fn try_recv(&self) -> Option<(String, Option<Prepared>)> {
        self.rx.try_recv().ok()
    }
}

/// Cache name of a prepared sound: the item and the tempo it follows.
pub fn prepared_id(key: &str, tempo: Option<f32>) -> String {
    match tempo {
        Some(bpm) => format!("{key}@{bpm:.2}"),
        None => key.to_string(),
    }
}

const RENDER_RATE: u32 = 48_000;

/// Loads (or renders) a preview sound. Runs off the interface thread.
pub fn prepare(source: &Source, tempo: Option<f32>) -> Option<Prepared> {
    match source {
        Source::Sample(path) => {
            let sample = super::compat::load_sample(&path.to_string_lossy()).ok()?;
            let bpm = ponzik_project::warp::detect_tempo(&sample);
            let loop_bpm = bpm.and_then(|b| ponzik_project::warp::loop_bpm(&sample, b, tempo.unwrap_or(120.0)));
            let (sample, synced) = match (loop_bpm, tempo) {
                (Some(from), Some(to)) if from > 0.0 && to > 0.0 => (stretched(&sample, from / to), true),
                _ => (sample, false),
            };
            Some(Prepared { sample: Arc::new(sample), is_loop: loop_bpm.is_some(), synced })
        }
        Source::Instrument { kind, melodic } => {
            let sample = render_instrument(kind, *melodic)?;
            Some(Prepared { sample: Arc::new(sample), is_loop: false, synced: false })
        }
    }
}

/// `sample` lasting `factor` times as long, same pitch.
fn stretched(sample: &Sample, factor: f32) -> Sample {
    if (factor - 1.0).abs() < 1e-3 {
        return sample.clone();
    }
    let right = if sample.right.len() == sample.left.len() { &sample.right } else { &sample.left };
    let (left, right) = ponzik_dsp::stretch::wsola_stereo(&sample.left, right, factor as f64, sample.sample_rate);
    Sample { name: sample.name.clone(), sample_rate: sample.sample_rate, left, right, markers: Vec::new() }
}

/// A short phrase of a native instrument: a C major chord held for a beat
/// and a half, or one hit of a drum instrument.
pub fn render_instrument(kind: &str, melodic: bool) -> Option<Sample> {
    ponzik_engine::registry::instrument_kind(kind)?;
    let mut project = ponzik_project::Project::empty();
    let ch = project.add_channel(kind, ponzik_project::InstrumentDef::new(kind), 60);
    let beat = ponzik_engine::PPQ;
    let keys: &[u8] = if melodic { &[60, 64, 67] } else { &[60] };
    let notes: Vec<ponzik_engine::Note> =
        keys.iter().map(|&key| ponzik_engine::Note { start: 0, len: beat * 3 / 2, key, vel: 0.8, ..ponzik_engine::Note::DEFAULT }).collect();
    let pattern = &mut project.patterns[0];
    pattern.len = beat * 2;
    pattern.notes.resize(ch + 1, Vec::new());
    pattern.notes[ch] = notes;
    let audio = ponzik_project::render_song(&project, RENDER_RATE, &|_| None, ponzik_project::RenderMode::Pattern(0, 1));
    // Two seconds at most: a preview, not a song.
    let frames = (audio.len() / 2).min(2 * RENDER_RATE as usize);
    let (left, right) = (0..frames).map(|i| (audio[2 * i], audio[2 * i + 1])).unzip();
    Some(Sample { name: kind.to_string(), sample_rate: RENDER_RATE as f32, left, right, markers: Vec::new() })
}

/// Prepared sounds kept, the oldest forgotten first.
pub const CACHE_MAX: usize = 24;

/// Recently prepared sounds, by [`prepared_id`].
#[derive(Default)]
pub struct PreviewCache {
    entries: HashMap<String, (Arc<Prepared>, u64)>,
    clock: u64,
}

impl PreviewCache {
    pub fn get(&mut self, id: &str) -> Option<Arc<Prepared>> {
        self.clock += 1;
        let clock = self.clock;
        self.entries.get_mut(id).map(|e| {
            e.1 = clock;
            e.0.clone()
        })
    }

    pub fn insert(&mut self, id: String, p: Prepared) -> Arc<Prepared> {
        self.clock += 1;
        let p = Arc::new(p);
        self.entries.insert(id, (p.clone(), self.clock));
        while self.entries.len() > CACHE_MAX {
            let Some(oldest) = self.entries.iter().min_by_key(|e| e.1.1).map(|e| e.0.clone()) else { break };
            self.entries.remove(&oldest);
        }
        p
    }
}

// --- In the interface ------------------------------------------------------------

/// Which items the browser lists, besides the category tabs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Filter {
    #[default]
    All,
    Favorites,
    Recent,
}

/// The browser's state in the interface.
#[derive(Default)]
pub struct BrowserUi {
    pub prefs: BrowserPrefs,
    pub preview: Preview,
    pub filter: Filter,
    /// Row picked with the arrow keys (index in the listed items, or in the
    /// rows of the tree).
    pub sel: Option<usize>,
    /// The list was clicked: the arrows and Enter walk it.
    pub list_focus: bool,
    /// Keys taken for the list this frame.
    pub keys: Vec<super::tree::NavKey>,
    /// A folder to open and bring into sight (picked among the search results).
    pub reveal: Option<String>,
    /// Folder under the pointer (or holding the row under it): its guide lights up.
    pub hot_folder: Option<usize>,
    /// Where the preferences are saved (none: not saved, in captures).
    pub path: Option<PathBuf>,
    preparer: Option<Preparer>,
    cache: PreviewCache,
    /// Prepared sound waited for, and the item it belongs to.
    waiting: Option<(String, String)>,
    /// When the voice was sent: it may not have started yet.
    sent_at: Option<f32>,
}

impl BrowserUi {
    /// A sound is on its way or playing: keep the frames coming.
    pub fn busy(&self) -> bool {
        self.waiting.is_some() || self.preview.playing.is_some()
    }
}

/// What a preview of `key` plays, if it can be previewed.
fn source_of(key: &str) -> Option<Source> {
    if let Some(path) = key.strip_prefix("sample:") {
        return Some(Source::Sample(PathBuf::from(path)));
    }
    let kind = key.strip_prefix("instrument:")?;
    native_sound(kind).then(|| Source::Instrument { kind: kind.to_string(), melodic: true })
}

/// A native instrument that sounds on its own (the samplers need a sample).
fn native_sound(kind: &str) -> bool {
    ponzik_engine::registry::instrument_kind(kind).is_some() && !super::devices::sample_kind(kind)
}

/// True when the browser can play `payload`: samples and native instruments.
pub fn previewable(payload: &Payload) -> bool {
    match payload {
        Payload::Sample(_) => true,
        Payload::Instrument { kind, .. } => native_sound(kind),
        Payload::Effect { .. } | Payload::Files(_) => false,
    }
}

impl super::app::App {
    /// Reads `browser.json` from the configuration folder; saved from now on.
    pub fn load_browser_prefs(&mut self) {
        let path = super::compat::config_dir().join("browser.json");
        self.browser.prefs = BrowserPrefs::load(&path);
        self.browser.path = Some(path);
    }

    pub fn save_browser_prefs(&self) {
        if let Some(path) = &self.browser.path
            && let Err(e) = self.browser.prefs.save(path)
        {
            eprintln!("browser preferences not saved ({}): {e}", path.display());
        }
    }

    pub fn toggle_favorite(&mut self, key: &str) {
        let on = self.browser.prefs.toggle_favorite(key);
        self.save_browser_prefs();
        self.toast(if on { ponzik_i18n::tr("Added to the favorites") } else { ponzik_i18n::tr("Removed from the favorites") });
    }

    /// An item of the browser was just used: first among the recent ones.
    pub fn note_recent(&mut self, payload: &Payload) {
        let key = item_key(payload);
        if self.browser.prefs.recent.first() != Some(&key) {
            self.browser.prefs.push_recent(&key);
            self.save_browser_prefs();
        }
    }

    pub fn set_auto_preview(&mut self, on: bool) {
        self.browser.prefs.auto_preview = on;
        let cmd = self.browser.preview.set_auto(on);
        self.run_preview(cmd);
        self.save_browser_prefs();
    }

    pub fn set_preview_volume(&mut self, volume: f32) {
        self.browser.prefs.volume = volume.clamp(0.0, 1.0);
        if self.browser.preview.playing.is_some() {
            let gain = self.browser.prefs.gain();
            self.send(ponzik_engine::Command::AuditionGain(gain));
        }
    }

    /// Carries out what the preview state machine asked for.
    pub fn run_preview(&mut self, cmd: Option<PreviewCmd>) {
        match cmd {
            Some(PreviewCmd::Play(key)) => self.start_preview(&key),
            Some(PreviewCmd::Stop) => self.stop_preview_voice(),
            None => {}
        }
    }

    /// Stops the preview (the browser closed, a drag began, the focus left).
    pub fn stop_preview(&mut self) {
        let cmd = self.browser.preview.stop();
        self.run_preview(cmd);
    }

    fn start_preview(&mut self, key: &str) {
        let Some(mut source) = source_of(key) else {
            self.browser.preview.finished();
            return;
        };
        if let Source::Instrument { kind, melodic } = &mut source {
            *melodic = self.is_melodic(kind);
        }
        // A loop follows the song while it plays.
        let tempo = (self.playing && matches!(source, Source::Sample(_)))
            .then(|| self.project.tempo_map().view().bpm_at(self.position) as f32)
            .filter(|b| b.is_finite() && *b > 0.0);
        let id = prepared_id(key, tempo);
        if let Some(p) = self.browser.cache.get(&id) {
            self.browser.waiting = None;
            self.play_prepared(&p);
            return;
        }
        if self.browser.preparer.is_none() {
            self.browser.preparer = Preparer::start();
        }
        // The sound of the previous row stops while this one loads.
        if self.browser.sent_at.take().is_some() {
            self.send(ponzik_engine::Command::Audition(None));
        }
        let Some(preparer) = &self.browser.preparer else { return };
        preparer.request(id.clone(), source, tempo);
        self.browser.waiting = Some((id, key.to_string()));
    }

    fn play_prepared(&mut self, p: &Prepared) {
        let len = p.sample.len();
        if len < 2 {
            self.browser.preview.finished();
            return;
        }
        self.lab_stop();
        let looped = p.is_loop.then_some((0, len));
        let mut voice = ponzik_engine::audition::Audition::new(p.sample.clone(), 0, len, looped, self.browser.prefs.gain());
        if p.synced {
            voice = voice.on_next_beat();
        }
        self.send(ponzik_engine::Command::Audition(Some(Box::new(voice))));
        self.browser.sent_at = Some(self.time);
    }

    fn stop_preview_voice(&mut self) {
        self.browser.waiting = None;
        if self.browser.sent_at.take().is_some() {
            self.send(ponzik_engine::Command::Audition(None));
        }
    }

    /// Picks up the prepared sounds and notices a preview that ended; true
    /// when the interface must redraw.
    pub fn browser_tick(&mut self) -> bool {
        let mut changed = false;
        while let Some((id, prepared)) = self.browser.preparer.as_ref().and_then(Preparer::try_recv) {
            changed = true;
            let wanted = self.browser.waiting.as_ref().is_some_and(|w| w.0 == id);
            match prepared {
                Some(p) => {
                    let p = self.browser.cache.insert(id, p);
                    if wanted {
                        self.browser.waiting = None;
                        self.play_prepared(&p);
                    }
                }
                None if wanted => {
                    self.browser.waiting = None;
                    self.browser.preview.finished();
                }
                None => {}
            }
        }
        // Ended on its own (a one-shot), or taken over by the Sample Lab.
        if self.browser.waiting.is_none()
            && self.browser.preview.playing.is_some()
            && self.browser.sent_at.is_none_or(|t| self.time - t > 0.4)
            && self.engine.audition_position().is_none()
        {
            self.browser.preview.finished();
            self.browser.sent_at = None;
            changed = true;
        }
        changed
    }

    /// `PONZIK_BROWSE=fav|recent|open:Drums,Drums/Kicks|q:kick|star:0,3|play:2|sel:1|focus|auto:off|vol:0.4`:
    /// the browser's filters, open folders, search, stars and preview for a capture.
    pub fn screenshot_browser(&mut self, spec: &str, entries: &[(usize, Payload)]) {
        for part in spec.split(';') {
            let (name, arg) = part.split_once(':').unwrap_or((part, ""));
            let nums = || arg.split(',').filter_map(|v| v.trim().parse::<usize>().ok()).collect::<Vec<_>>();
            match name.trim() {
                "fav" => self.browser.filter = Filter::Favorites,
                "recent" => {
                    self.browser.filter = Filter::Recent;
                    for i in nums().into_iter().rev() {
                        if let Some((_, p)) = entries.get(i) {
                            self.browser.prefs.push_recent(&item_key(p));
                        }
                    }
                }
                "open" => {
                    for path in arg.split(',').map(str::trim).filter(|p| !p.is_empty()) {
                        let prefix = super::tree::TAB_PREFIX[self.sidebar_tab.min(3)];
                        self.browser.prefs.set_open(&format!("{prefix}/{path}"), true);
                    }
                }
                "q" => self.sidebar_query = arg.to_string(),
                "focus" => self.browser.list_focus = true,
                "star" => {
                    for i in nums() {
                        if let Some((_, p)) = entries.get(i) {
                            self.browser.prefs.toggle_favorite(&item_key(p));
                        }
                    }
                }
                "play" => {
                    if let Some((_, p)) = nums().first().and_then(|&i| entries.get(i)) {
                        self.browser.preview.playing = Some(item_key(p));
                    }
                }
                "sel" => self.browser.sel = nums().first().copied(),
                "auto" => self.browser.prefs.auto_preview = arg.trim() != "off",
                "vol" => self.browser.prefs.volume = arg.trim().parse::<f32>().map_or(0.7, |v| v.clamp(0.0, 1.0)),
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(p: &str) -> Vec<Tag> {
        infer_tags(Path::new(p)).iter().collect()
    }

    #[test]
    fn tags_come_from_folders_and_names() {
        assert_eq!(tags("/lib/Drums/Kicks/Kick - 808 Long 01.wav"), [Tag::Kick]);
        assert_eq!(tags("/lib/Drums/Hats/Open Hat - Wide 01.wav"), [Tag::Hat]);
        assert_eq!(tags("/lib/Drums/Rims/Rim - Tight 01.wav"), [Tag::Snare]);
        assert_eq!(tags("/lib/Drums/Shakers/Shaker 01.wav"), [Tag::Perc]);
        assert_eq!(tags("/lib/Bass/Synth/Reese - Dark - E1.wav"), [Tag::Bass]);
        assert_eq!(tags("/lib/Bass/808/808 - Long - C1.wav"), [Tag::Bass]);
        assert_eq!(tags("/lib/Loops/DnB/DnB Roller 01 - 174 BPM.wav"), [Tag::Loop]);
        assert_eq!(tags("/lib/FX/Risers/Riser - White Noise 01.wav"), [Tag::Fx]);
        assert_eq!(tags("/lib/Synths/Pads/Pad - Glass - Am9.wav"), [Tag::Pad]);
        assert_eq!(tags("/lib/Synths/Plucks/Pluck - Harp - C5.wav"), [Tag::Synth]);
        assert_eq!(tags("/lib/Synths/Keys/Keys - EPiano - Cmaj9.wav"), [Tag::Keys]);
    }

    #[test]
    fn tags_from_loose_file_names() {
        // Camel case, abbreviations, tempo glued to "bpm", several tags.
        assert_eq!(tags("/home/me/Downloads/OpenHat_03.wav"), [Tag::Hat]);
        assert_eq!(tags("/x/y/BD_hard.wav"), [Tag::Kick]);
        assert_eq!(tags("/x/y/funky_drums_120bpm.wav"), [Tag::Loop]);
        assert_eq!(tags("/x/y/vox chop.wav"), [Tag::Vocal]);
        assert_eq!(tags("/x/y/Kick Loop 128.wav"), [Tag::Kick, Tag::Loop]);
        assert_eq!(tags("/x/y/sub bass kick.wav"), [Tag::Kick, Tag::Bass]);
        // Nothing to go on; a word that merely contains a key stays untagged.
        assert!(tags("/x/y/take 3.wav").is_empty());
        assert!(tags("/x/y/sdcard dump.wav").is_empty());
        assert_eq!(words("OpenHat-Closed_hh 909"), ["open", "hat", "closed", "hh", "909"]);
    }

    #[test]
    fn favorites_and_recent_round_trip() {
        let mut p = BrowserPrefs::default();
        assert!(p.toggle_favorite("sample:/a.wav"));
        assert!(p.toggle_favorite("instrument:pulse"));
        assert!(!p.toggle_favorite("sample:/a.wav"));
        assert!(p.toggle_favorite("effect:reverb"));
        assert!(p.is_favorite("effect:reverb") && !p.is_favorite("sample:/a.wav"));
        for i in 0..40 {
            p.push_recent(&format!("sample:/{i}.wav"));
        }
        p.push_recent("sample:/5.wav");
        p.push_recent("");
        assert_eq!(p.recent.len(), RECENT_MAX);
        assert_eq!(p.recent_rank("sample:/5.wav"), Some(0));
        assert_eq!(p.recent_rank("sample:/39.wav"), Some(1));
        assert_eq!(p.recent_rank("sample:/0.wav"), None, "the oldest fell off");
        p.auto_preview = false;
        p.volume = 0.25;

        let dir = std::env::temp_dir().join(format!("ponzik-browser-{}", std::process::id()));
        let path = dir.join("sub").join("browser.json");
        p.save(&path).unwrap();
        assert_eq!(BrowserPrefs::load(&path), p);
        assert!(!path.with_extension("json.tmp").exists());
        std::fs::remove_dir_all(&dir).unwrap();
        // Missing or damaged file: the defaults.
        assert_eq!(BrowserPrefs::load(&path), BrowserPrefs::default());
        let j = json::parse(r#"{"favorites": ["a", 3, "a", ""], "preview_volume": 7, "auto_preview": "x"}"#).unwrap();
        let q = BrowserPrefs::from_json(&j);
        assert_eq!(q.favorites, ["a"]);
        assert_eq!((q.volume, q.auto_preview), (1.0, true));
    }

    #[test]
    fn folder_states_round_trip() {
        let mut p = BrowserPrefs::default();
        // Sounds closed, devices open, until touched.
        assert!(!p.is_open("s/Drums") && p.is_open("i/Synths"));
        p.set_open("s/Drums", true);
        p.set_open("s/Drums/Kicks", true);
        p.set_open("e/Space", false);
        assert!(p.is_open("s/Drums") && p.is_open("s/Drums/Kicks") && !p.is_open("e/Space"));
        // Back to its default: forgotten.
        p.set_open("s/Drums/Kicks", false);
        assert_eq!(p.folders.len(), 2);
        let back = BrowserPrefs::from_json(&json::parse(&p.to_json().pretty()).unwrap());
        assert_eq!(back, p);
        // Damaged entries are skipped, older files have none.
        let j = json::parse(r#"{"folders": {"s/FX": true, "s/Bass": 3, "": true}}"#).unwrap();
        assert_eq!(BrowserPrefs::from_json(&j).folders, [("s/FX".to_string(), true)]);
        assert!(BrowserPrefs::from_json(&json::parse("{}").unwrap()).folders.is_empty());
    }

    #[test]
    fn item_keys_tell_kinds_apart() {
        let s = item_key(&Payload::Sample(PathBuf::from("/a/b.wav")));
        let i = item_key(&Payload::Instrument { kind: "pulse".into(), name: "Pulse".into() });
        let e = item_key(&Payload::Effect { kind: "pulse".into(), name: "Pulse".into() });
        assert_eq!(s, "sample:/a/b.wav");
        assert_ne!(i, e);
    }

    #[test]
    fn hover_plays_after_the_dwell_and_stops_on_leave() {
        let mut p = Preview::default();
        assert_eq!(p.hover(Some("a"), 0.0, true), None, "just arrived");
        assert_eq!(p.hover(Some("a"), 0.05, true), None);
        assert_eq!(p.hover(Some("a"), 0.2, true), Some(PreviewCmd::Play("a".into())));
        assert_eq!(p.hover(Some("a"), 0.5, true), None, "once");
        // Ended on its own: not again while the pointer stays.
        p.finished();
        assert_eq!(p.hover(Some("a"), 2.0, true), None);
        // Sweeping across rows stays quiet.
        assert_eq!(p.hover(Some("b"), 2.0, true), None);
        assert_eq!(p.hover(Some("c"), 2.05, true), None);
        assert_eq!(p.hover(Some("c"), 2.2, true), Some(PreviewCmd::Play("c".into())));
        // Moving to another row stops the sound, then the new one plays.
        assert_eq!(p.hover(Some("d"), 2.3, true), Some(PreviewCmd::Stop));
        assert_eq!(p.hover(Some("d"), 2.5, true), Some(PreviewCmd::Play("d".into())));
        // Leaving the list stops it.
        assert_eq!(p.hover(None, 2.6, true), Some(PreviewCmd::Stop));
        assert_eq!(p.hover(None, 3.0, true), None);
        assert_eq!(p.playing, None);
    }

    #[test]
    fn auto_toggle_and_play_button() {
        let mut p = Preview::default();
        // Auto-preview off: hovering is silent, the play button works.
        assert_eq!(p.hover(Some("a"), 0.0, false), None);
        assert_eq!(p.hover(Some("a"), 1.0, false), None);
        assert_eq!(p.click_play("a"), PreviewCmd::Play("a".into()));
        assert_eq!(p.click_play("a"), PreviewCmd::Stop, "second click stops");
        assert_eq!(p.click_play("a"), PreviewCmd::Play("a".into()));
        // Switched off while playing: stops.
        assert_eq!(p.set_auto(false), Some(PreviewCmd::Stop));
        assert_eq!(p.set_auto(false), None);
        // Switched on with the pointer on a row: it plays without a new dwell.
        assert_eq!(p.set_auto(true), None);
        assert_eq!(p.hover(Some("a"), 1.01, true), Some(PreviewCmd::Play("a".into())));
        // Arrow keys play at once, only with auto-preview.
        assert_eq!(p.select("b", true), Some(PreviewCmd::Play("b".into())));
        assert_eq!(p.select("c", false), None);
        // The pointer resting elsewhere does not cut an arrow-picked sound...
        let mut q = Preview::default();
        assert_eq!(q.hover(None, 0.0, true), None);
        assert_eq!(q.select("x", true), Some(PreviewCmd::Play("x".into())));
        assert_eq!(q.hover(None, 1.0, true), None);
        // ...but stop() does.
        assert_eq!(q.stop(), Some(PreviewCmd::Stop));
        assert_eq!(q.stop(), None);
    }

    #[test]
    fn instruments_render_a_short_phrase() {
        let s = render_instrument("pulse", true).expect("a native synth");
        assert!(s.len() > RENDER_RATE as usize / 2 && s.len() <= 2 * RENDER_RATE as usize, "{}", s.len());
        assert!(s.left.iter().any(|v| v.abs() > 0.01), "audible");
        assert!(render_instrument("clap:unknown", true).is_none());
    }

    #[test]
    fn loops_stretch_to_the_song_tempo() {
        // Four beats of clicks at 120 BPM: a one-bar loop.
        let sr = 48_000.0;
        let mut left = vec![0.0f32; 96_000];
        for beat in 0..4 {
            for i in 0..400 {
                left[beat * 24_000 + i] = 0.8 * (1.0 - i as f32 / 400.0) * if i % 2 == 0 { 1.0 } else { -1.0 };
            }
        }
        let sample = Sample { name: "loop".into(), sample_rate: sr, right: left.clone(), left, markers: Vec::new() };
        let slower = stretched(&sample, 120.0 / 100.0);
        assert!((slower.len() as f32 - 96_000.0 * 1.2).abs() < 50.0, "{}", slower.len());
        assert_eq!(stretched(&sample, 1.0).len(), sample.len());
        assert_eq!(prepared_id("sample:/a.wav", Some(100.0)), "sample:/a.wav@100.00");
        assert_eq!(prepared_id("sample:/a.wav", None), "sample:/a.wav");
    }

    #[test]
    fn cache_forgets_the_oldest() {
        let mut c = PreviewCache::default();
        let s = || Prepared { sample: Arc::new(Sample { name: String::new(), sample_rate: 48_000.0, left: vec![], right: vec![], markers: vec![] }), is_loop: false, synced: false };
        for i in 0..CACHE_MAX {
            c.insert(i.to_string(), s());
        }
        assert!(c.get("0").is_some(), "used again: stays");
        c.insert("new".into(), s());
        assert!(c.get("0").is_some());
        assert!(c.get("1").is_none(), "least recently used went");
        assert!(c.get("new").is_some());
    }
}
