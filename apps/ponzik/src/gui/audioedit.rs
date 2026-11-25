//! The audio editor, Ponzik's Edison: the Sample Lab's second mode.
//!
//! Any audio clip, sample or recording opens here as an *edit copy*: the file
//! on disk is never touched. Every operation rewrites the copy and leaves an
//! undo step (Ctrl+Z, Ctrl+Y); what leaves the editor (a new file, a channel,
//! a playlist clip, the source replaced) goes through the project's history.
//!
//! - big zoomable waveform (min/max peaks far out, every sample up close),
//!   or a spectrogram; selection snapped to zero crossings or beats; markers,
//!   regions; play the selection, looped or not;
//! - edits: cut, copy, paste, delete, crop, insert silence; gain, peak or
//!   LUFS normalize, fades with five shapes, reverse, DC offset; noise
//!   reduction from a learnt profile, declick, declip, trim silence; time
//!   stretch and pitch shift (the warp engine), seamless loop, sample rate
//!   and channel conversion;
//! - slicing: transients (sensitivity, minimum gap), hand-editable slice
//!   markers, tempo detection; slices to the Slicer, to Drum Pads, to MIDI,
//!   to a drum pattern rebuilt from a break, or to files;
//! - a free recording straight into the editor.
//!
//! The drawing is in [`super::audioedit_view`].

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ponzik_dsp::denoise::{self, DenoiseSettings, NoiseProfile};
use ponzik_dsp::edit::{self, FadeShape};
use ponzik_dsp::onset::{self, OnsetAnalysis};
use ponzik_dsp::tempo::{self, TempoEstimate};
use ponzik_dsp::warp::WarpAlgo;
use ponzik_engine::audition::Audition;
use ponzik_engine::input::RecordBuffer;
use ponzik_engine::{Command, InputSource, Note, PPQ, Sample, TICKS_PER_BAR};
use ponzik_i18n::{tr, tr_fmt};
use ponzik_project::InstrumentDef;
use ponzik_window::{Key, Modifiers};

use super::app::{App, Focus, View};
use super::samplelab::Source;

/// Frames per min/max peak.
pub(super) const BUCKET: usize = 64;
/// Shortest view, in frames: a few samples across the whole width.
pub(super) const MIN_VIEW: f64 = 24.0;
/// Undo steps kept.
const UNDO_STEPS: usize = 64;
/// Longest free recording, in seconds.
pub(super) const REC_SECONDS: f32 = 180.0;
/// The Slicer plays 64 slices at most; Drum Pads have 16 pads.
const MAX_SLICES: usize = 64;
const PADS: usize = 16;
/// Slicer "Mode" parameter set to "Markers".
const SLICER_MODE: u32 = 0;
const SLICER_MARKERS: f32 = 2.0;
/// Zero crossing search radius when snapping, in seconds.
const ZERO_SNAP: f32 = 0.004;

/// Tool tab of the side panel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Tab {
    #[default]
    Volume,
    Clean,
    Time,
    Slices,
}

impl Tab {
    pub(super) const ALL: [Tab; 4] = [Tab::Volume, Tab::Clean, Tab::Time, Tab::Slices];
}

/// How Normalize measures.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum NormMode {
    #[default]
    Peak,
    Lufs,
}

/// A named position.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Marker {
    pub pos: usize,
    pub name: String,
}

/// A named span.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Region {
    pub start: usize,
    pub end: usize,
    pub name: String,
}

/// What is edited, and undone as a whole.
#[derive(Clone)]
pub(super) struct Doc {
    /// The edit copy.
    pub sample: Arc<Sample>,
    /// Selection `[a, b)`, `a < b`.
    pub sel: Option<(usize, usize)>,
    /// Insertion point (where Paste goes without a selection, where playing starts).
    pub cursor: usize,
    pub markers: Vec<Marker>,
    pub regions: Vec<Region>,
    /// Slice cuts, sorted, strictly inside the sound.
    pub slices: Vec<usize>,
    /// Tempo of the sound, detected or set.
    pub bpm: Option<f32>,
}

impl Doc {
    pub(super) fn new(sample: Arc<Sample>) -> Self {
        let len = sample.len();
        let slices = sample.markers.iter().copied().filter(|&m| m > 0 && m < len).collect();
        Self { sample, sel: None, cursor: 0, markers: Vec::new(), regions: Vec::new(), slices, bpm: None }
    }

    pub(super) fn len(&self) -> usize {
        self.sample.len()
    }

    /// The selection, or the whole sound.
    pub(super) fn range(&self) -> (usize, usize) {
        self.sel.filter(|(a, b)| b > a).unwrap_or((0, self.len()))
    }

    /// Slice bounds: 0, the cuts, the end.
    pub(super) fn bounds(&self) -> Vec<usize> {
        let mut v = vec![0];
        v.extend(self.slices.iter().copied().filter(|&m| m > 0 && m < self.len()));
        v.push(self.len());
        v
    }

    /// After `[a, b)` became `new_len` frames long: positions after it move,
    /// positions inside scale (or vanish when the span was deleted).
    fn remap(&mut self, a: usize, b: usize, new_len: usize) {
        let map = |p: usize| -> Option<usize> {
            if p < a {
                Some(p)
            } else if p >= b {
                Some(p - (b - a) + new_len)
            } else if new_len == 0 {
                None
            } else {
                Some(a + ((p - a) as f64 * new_len as f64 / (b - a).max(1) as f64) as usize)
            }
        };
        self.slices = self.slices.iter().filter_map(|&p| map(p)).collect();
        self.slices.dedup();
        self.markers.retain_mut(|m| map(m.pos).map(|p| m.pos = p).is_some());
        self.regions.retain_mut(|r| match (map(r.start), map(r.end.saturating_sub(1))) {
            (Some(s), Some(e)) if e + 1 > s => {
                (r.start, r.end) = (s, e + 1);
                true
            }
            _ => false,
        });
        self.cursor = map(self.cursor).unwrap_or(a);
    }

    /// Replaces `[a, b)` with `l`/`r`; markers follow, the new part is selected.
    pub(super) fn splice(&mut self, a: usize, b: usize, l: Vec<f32>, r: Vec<f32>) {
        let n = l.len();
        let s = &self.sample;
        let join = |v: &[f32], mid: Vec<f32>| [&v[..a], &mid[..], &v[b..]].concat();
        let new = Sample { left: join(&s.left, l), right: join(&s.right, r), markers: Vec::new(), ..(**s).clone() };
        self.sample = Arc::new(new);
        self.remap(a, b, n);
        self.sel = (n > 0).then_some((a, a + n));
        self.cursor = a;
    }

    /// Runs `f` on both channels of `[a, b)` in place (same length).
    pub(super) fn process(&mut self, a: usize, b: usize, mut f: impl FnMut(&mut [f32])) {
        let mut s = (*self.sample).clone();
        f(&mut s.left[a..b]);
        f(&mut s.right[a..b]);
        self.sample = Arc::new(s);
    }

    /// The sound with its slice cuts as markers, ready to save.
    pub(super) fn export(&self) -> Sample {
        let mut s = (*self.sample).clone();
        s.markers = self.slices.clone();
        s
    }
}

/// Min/max peaks of the edit copy, per channel, recomputed when it changes.
pub(super) struct Peaks {
    pub key: usize,
    pub left: Vec<(f32, f32)>,
    pub right: Vec<(f32, f32)>,
    pub mono: bool,
}

impl Peaks {
    pub(super) fn new(s: &Arc<Sample>) -> Self {
        let fold = |v: &[f32]| v.chunks(BUCKET).map(|c| c.iter().fold((0.0f32, 0.0f32), |(lo, hi), &x| (lo.min(x), hi.max(x)))).collect();
        Self { key: Arc::as_ptr(s) as usize, left: fold(&s.left), right: fold(&s.right), mono: s.is_mono() }
    }
}

/// Transient analysis of the edit copy.
pub(super) struct Onsets {
    pub key: usize,
    pub mono: Vec<f32>,
    pub analysis: OnsetAnalysis,
}

/// Spectrogram picture: three opacity masks (deep, mid, hot) in physical pixels.
pub(super) struct SpecImage {
    pub key: (usize, u64, u64, usize, usize),
    pub w: usize,
    pub h: usize,
    pub masks: [Vec<u8>; 3],
}

/// A gesture in progress.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Grab {
    /// Selecting from this frame.
    Select(usize),
    /// Moving one end of the selection (true: the start).
    Edge(bool),
    /// Moving slice cut `k`.
    Slice(usize),
    Marker(usize),
    Overview,
    /// Panning with the middle button: view start and pointer x at the press.
    Pan(f64, f32),
    /// A tool slider.
    Slider,
}

/// Free recording in progress.
pub(super) struct Capture {
    pub started: f32,
    /// Input level per frame drawn, 0 to 1, to draw the take as it grows.
    pub levels: Vec<f32>,
}

/// Result line under the toolbar, after an operation.
pub(super) struct Status {
    pub text: String,
    pub at: f32,
}

pub struct AudioEditor {
    /// The Lab shows the editor rather than the chopper.
    pub active: bool,
    pub source: Source,
    pub path: Option<String>,
    pub(super) name: String,
    pub(super) doc: Option<Doc>,
    undo: Vec<(String, Doc)>,
    redo: Vec<(String, Doc)>,
    pub(super) clipboard: Option<(Vec<f32>, Vec<f32>)>,
    /// Target view (start, length in frames) and the view shown, easing to it.
    pub(super) view: (f64, f64),
    pub(super) shown: (f64, f64),
    pub(super) last_time: f32,
    pub(super) grab: Option<Grab>,
    /// Playing: the start of the audition voice in the edit copy.
    pub(super) playing: bool,
    pub(super) looping: bool,
    pub(super) snap_zero: bool,
    pub(super) snap_beat: bool,
    pub(super) spectrum: bool,
    pub(super) tab: Tab,
    /// Scroll of the tool panel, and the height of its content last frame.
    pub(super) panel_scroll: f32,
    pub(super) panel_content: f32,
    pub(super) peaks: Option<Peaks>,
    pub(super) onsets: Option<Onsets>,
    pub(super) spec: Option<SpecImage>,
    // Tool settings.
    pub(super) gain_db: f32,
    pub(super) norm: NormMode,
    pub(super) peak_target: f32,
    pub(super) lufs_target: f32,
    pub(super) fade: FadeShape,
    pub(super) noise: Option<NoiseProfile>,
    pub(super) denoise: DenoiseSettings,
    pub(super) declick: f32,
    pub(super) trim_db: f32,
    pub(super) stretch: f32,
    pub(super) semitones: f32,
    pub(super) algo: WarpAlgo,
    pub(super) xfade_ms: f32,
    pub(super) rate: u32,
    pub(super) silence_ms: f32,
    pub(super) sensitivity: f32,
    pub(super) min_gap_ms: f32,
    pub(super) tempo: Option<TempoEstimate>,
    pub(super) capture: Option<Capture>,
    pub(super) note: Option<Status>,
    /// Peak and RMS of the selection: (sample, start, end, peak, rms).
    pub(super) stats: Option<(usize, usize, usize, f32, f32)>,
    /// Last undo step: label and time, so a gesture makes one step.
    last_edit: Option<(String, f32)>,
    /// Count of free recordings, for their names.
    takes: u32,
}

impl Default for AudioEditor {
    fn default() -> Self {
        Self {
            active: false,
            source: Source::Browser,
            path: None,
            name: String::new(),
            doc: None,
            undo: Vec::new(),
            redo: Vec::new(),
            clipboard: None,
            view: (0.0, 1.0),
            shown: (0.0, 1.0),
            last_time: 0.0,
            grab: None,
            playing: false,
            looping: false,
            snap_zero: true,
            snap_beat: false,
            spectrum: false,
            tab: Tab::Volume,
            panel_scroll: 0.0,
            panel_content: 0.0,
            peaks: None,
            onsets: None,
            spec: None,
            gain_db: 0.0,
            norm: NormMode::Peak,
            peak_target: -0.3,
            lufs_target: -14.0,
            fade: FadeShape::EqualPower,
            noise: None,
            denoise: DenoiseSettings::default(),
            declick: 0.5,
            trim_db: -48.0,
            stretch: 1.0,
            semitones: 0.0,
            algo: WarpAlgo::Tones,
            xfade_ms: 30.0,
            rate: 48_000,
            silence_ms: 500.0,
            sensitivity: 0.5,
            min_gap_ms: 45.0,
            tempo: None,
            capture: None,
            note: None,
            stats: None,
            last_edit: None,
            takes: 0,
        }
    }
}

impl AudioEditor {
    /// Something moves on screen (recording, playing).
    pub fn needs_redraw(&self) -> bool {
        self.active && (self.capture.is_some() || self.playing)
    }
}

/// Folder of the editor's files: next to the project ("Audio/<project>"),
/// else Ponzik's Samples folder.
fn audio_dir(app: &App) -> PathBuf {
    match app.project_path.as_ref().and_then(|p| p.parent()) {
        Some(folder) => folder.join("Audio").join(super::compat::file_name(&app.project.name)),
        None => super::compat::projects_dir().join("Samples"),
    }
}

/// `dir/stem.ext`, or `dir/stem 2.ext`… when taken.
fn unique_path(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let first = dir.join(format!("{stem}.{ext}"));
    if !first.exists() {
        return first;
    }
    (2..10_000).map(|n| dir.join(format!("{stem} {n}.{ext}"))).find(|p| !p.exists()).unwrap_or(first)
}

/// FLAC file of a sample, 24 bits (mono when both channels match).
pub(super) fn encode_flac(s: &Sample) -> Vec<u8> {
    let mono = s.is_mono();
    let channels = if mono { 1 } else { 2 };
    let q = |v: f32| (v.clamp(-1.0, 1.0) * 8_388_607.0).round() as i32;
    let mut pcm = Vec::with_capacity(s.len() * channels);
    for i in 0..s.len() {
        pcm.push(q(s.left[i]));
        if !mono {
            pcm.push(q(s.right[i]));
        }
    }
    let rate = s.sample_rate.round().max(1.0) as u32;
    ponzik_engine::flac::encode(&pcm, channels, rate, 24, ponzik_engine::flac::EncodeOptions::default())
}

/// Kind of drum hit, told from the first 60 ms of a slice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Hit {
    Kick,
    Snare,
    Hat,
}

impl Hit {
    /// General MIDI note (and Drum Pads pad = note − 36).
    pub(super) fn note(self) -> u8 {
        match self {
            Hit::Kick => 36,
            Hit::Snare => 38,
            Hit::Hat => 42,
        }
    }
}

/// Classifies a drum hit by where its energy sits: mostly under 150 Hz is a
/// kick, mostly over 5 kHz a hat, anything in between a snare.
pub(super) fn classify(x: &[f32], sample_rate: f32) -> Hit {
    let n = ((0.06 * sample_rate) as usize).min(x.len());
    let x = &x[..n];
    let lp = |fc: f32| {
        let a = (-std::f32::consts::TAU * fc / sample_rate).exp();
        let mut y = 0.0f32;
        x.iter().map(move |&v| {
            y = (1.0 - a) * v + a * y;
            y
        })
    };
    let total: f32 = x.iter().map(|v| v * v).sum::<f32>().max(1e-12);
    // Two one-pole stages: a steeper low band.
    let low: Vec<f32> = lp(150.0).collect();
    let a = (-std::f32::consts::TAU * 150.0 / sample_rate).exp();
    let mut y = 0.0f32;
    let low_e: f32 = low.iter().map(|&v| {
        y = (1.0 - a) * v + a * y;
        y * y
    }).sum();
    let mid: Vec<f32> = lp(5_000.0).collect();
    let high_e: f32 = x.iter().zip(&mid).map(|(v, m)| (v - m) * (v - m)).sum();
    let (low, high) = (low_e / total, high_e / total);
    if low > 0.35 {
        Hit::Kick
    } else if high > 0.3 {
        Hit::Hat
    } else {
        Hit::Snare
    }
}

impl App {
    // --- Opening ---

    /// Opens a file in the audio editor.
    pub fn open_in_editor(&mut self, path: &str, source: Source) {
        if !self.ensure_sample(path) {
            return;
        }
        let sample = self.samples[path].clone();
        self.open_sample_in_editor(sample, Some(path.to_string()), source);
    }

    /// Opens a sample in the audio editor (a copy is edited, never the original).
    pub fn open_sample_in_editor(&mut self, sample: Arc<Sample>, path: Option<String>, source: Source) {
        self.ae_stop();
        let name = path
            .as_deref()
            .and_then(|p| Path::new(p).file_stem())
            .map_or_else(|| sample.name.clone(), |s| s.to_string_lossy().into_owned());
        let len = sample.len().max(1) as f64;
        let doc = Doc::new(sample);
        let ae = &mut self.audio_edit;
        ae.source = source;
        ae.path = path;
        ae.name = name;
        ae.undo.clear();
        ae.redo.clear();
        ae.view = (0.0, len);
        ae.shown = (0.0, len);
        ae.grab = None;
        ae.noise = None;
        ae.tempo = None;
        ae.note = None;
        ae.spec = None;
        ae.rate = doc.sample.sample_rate.round() as u32;
        ae.doc = Some(doc);
        ae.active = true;
        self.ae_refresh();
        self.ae_detect_tempo(false);
        self.set_view(View::Lab);
    }

    /// Opens what a channel plays (Sampler, Slicer…).
    pub fn open_channel_in_editor(&mut self, ch: usize) {
        if let Some(path) = self.project.channels.get(ch).and_then(|c| c.instrument.sample.clone()) {
            self.open_in_editor(&path, Source::Channel(ch));
        }
    }

    /// A file dropped on the Lab opens in the mode shown.
    pub fn open_in_lab_mode(&mut self, path: &str) {
        if self.audio_edit.active { self.open_in_editor(path, Source::Browser) } else { self.open_in_lab(path, Source::Browser) }
    }

    /// Switches the Lab between chopping and editing; the sound open in one
    /// follows to the other when that one is empty.
    pub(super) fn ae_set_active(&mut self, on: bool) {
        if on == self.audio_edit.active {
            return;
        }
        self.ae_stop();
        self.lab_stop();
        self.audio_edit.active = on;
        if on && self.audio_edit.doc.is_none() {
            if let Some(s) = self.lab.doc_sample() {
                let (path, source) = (self.lab.path.clone(), self.lab.source);
                self.open_sample_in_editor(s, path, source);
            }
        } else if !on && self.lab.doc_sample().is_none() {
            let ae = &self.audio_edit;
            if let Some(doc) = ae.doc.as_ref() {
                let (s, path, source) = (Arc::new(doc.export()), ae.path.clone(), ae.source);
                self.open_sample_in_lab(s, path, source);
            }
        }
    }

    /// Recomputes the analyses that depend on the edit copy, and keeps the
    /// view, selection and markers inside it.
    pub(super) fn ae_refresh(&mut self) {
        let Some(doc) = self.audio_edit.doc.as_mut() else { return };
        let len = doc.len();
        doc.sel = doc.sel.map(|(a, b)| (a.min(len), b.min(len))).filter(|(a, b)| b > a);
        doc.cursor = doc.cursor.min(len);
        doc.slices.retain(|&m| m > 0 && m < len);
        doc.slices.sort_unstable();
        doc.slices.dedup();
        doc.markers.retain(|m| m.pos <= len);
        let key = Arc::as_ptr(&doc.sample) as usize;
        let sample = doc.sample.clone();
        let ae = &mut self.audio_edit;
        if ae.peaks.as_ref().is_none_or(|p| p.key != key) {
            ae.peaks = Some(Peaks::new(&sample));
        }
        let lenf = len.max(1) as f64;
        ae.view.1 = ae.view.1.clamp(MIN_VIEW.min(lenf), lenf);
        ae.view.0 = ae.view.0.clamp(0.0, (lenf - ae.view.1).max(0.0));
        ae.shown.1 = ae.shown.1.min(lenf);
    }

    /// Onset analysis of the edit copy, made on demand.
    pub(super) fn ae_onsets(&mut self) -> Option<&Onsets> {
        let doc = self.audio_edit.doc.as_ref()?;
        let key = Arc::as_ptr(&doc.sample) as usize;
        if self.audio_edit.onsets.as_ref().is_none_or(|o| o.key != key) {
            let mono = onset::mono(&doc.sample.left, &doc.sample.right);
            let analysis = OnsetAnalysis::new(&mono, doc.sample.sample_rate);
            self.audio_edit.onsets = Some(Onsets { key, mono, analysis });
        }
        self.audio_edit.onsets.as_ref()
    }

    // --- Undo ---

    /// Undo point before an edit; the same edit repeated within a second
    /// (a slider being dragged) stays one step.
    fn ae_checkpoint(&mut self, label: &str) {
        let Some(doc) = self.audio_edit.doc.clone() else { return };
        let now = self.time;
        let ae = &mut self.audio_edit;
        let merge = ae.last_edit.as_ref().is_some_and(|(l, t)| l == label && now - t < 1.0 && label == tr("Detect slices"));
        if !merge {
            ae.undo.push((label.to_string(), doc));
            if ae.undo.len() > UNDO_STEPS {
                ae.undo.remove(0);
            }
        }
        ae.redo.clear();
        ae.last_edit = Some((label.to_string(), now));
    }

    pub(super) fn ae_undo(&mut self, redo: bool) -> bool {
        let ae = &mut self.audio_edit;
        let (from, to) = if redo { (&mut ae.redo, &mut ae.undo) } else { (&mut ae.undo, &mut ae.redo) };
        let Some((label, doc)) = from.pop() else { return false };
        if let Some(cur) = ae.doc.replace(doc) {
            to.push((label.clone(), cur));
        }
        ae.last_edit = None;
        self.ae_stop();
        self.ae_refresh();
        self.ae_note(&if redo { tr_fmt!("Redone: {}", label) } else { tr_fmt!("Undone: {}", label) });
        true
    }

    pub(super) fn ae_can_undo(&self, redo: bool) -> bool {
        if redo { !self.audio_edit.redo.is_empty() } else { !self.audio_edit.undo.is_empty() }
    }

    /// Applies an edit with an undo step; `edit` returns the message to show,
    /// or `None` when it changed nothing (the step is dropped).
    pub(super) fn ae_edit(&mut self, label: &str, edit: impl FnOnce(&mut Doc, &mut AudioEditor) -> Option<String>) -> bool {
        let Some(mut doc) = self.audio_edit.doc.clone() else { return false };
        self.ae_stop();
        match edit(&mut doc, &mut self.audio_edit) {
            Some(msg) => {
                self.ae_checkpoint(label);
                self.audio_edit.doc = Some(doc);
                self.ae_refresh();
                self.ae_note(&msg);
                true
            }
            None => false,
        }
    }

    pub(super) fn ae_note(&mut self, text: &str) {
        self.audio_edit.note = Some(Status { text: text.to_string(), at: self.time });
    }

    // --- Playing ---

    /// Plays `[from, to)` of the edit copy, looped or not.
    pub(super) fn ae_play_range(&mut self, from: usize, to: usize, looped: bool) {
        let Some(doc) = self.audio_edit.doc.as_ref() else { return };
        let s = doc.sample.clone();
        let (from, to) = (from.min(s.len()), to.min(s.len()));
        if to <= from + 1 {
            return;
        }
        let voice = if looped { Audition::new(s.clone(), from, s.len(), Some((from, to)), 1.0) } else { Audition::new(s, from, to, None, 1.0) };
        self.send(Command::Audition(Some(Box::new(voice))));
        self.audio_edit.playing = true;
    }

    /// Space: the selection (looped if Loop is on), else from the cursor.
    pub(super) fn ae_toggle_play(&mut self) {
        if self.ae_playhead().is_some() {
            self.ae_stop();
            return;
        }
        let Some(doc) = self.audio_edit.doc.as_ref() else { return };
        let looping = self.audio_edit.looping;
        match doc.sel {
            Some((a, b)) => self.ae_play_range(a, b, looping),
            None => {
                let (from, len) = (doc.cursor, doc.len());
                let from = if from + 1 >= len { 0 } else { from };
                self.ae_play_range(from, len, looping && from == 0);
            }
        }
    }

    pub fn ae_stop(&mut self) {
        if std::mem::take(&mut self.audio_edit.playing) {
            self.send(Command::Audition(None));
        }
    }

    /// Play position in the edit copy.
    pub(super) fn ae_playhead(&self) -> Option<f64> {
        if !self.audio_edit.playing {
            return None;
        }
        self.engine.audition_position()
    }

    // --- Selection and view ---

    /// Snaps a frame to the nearest beat (when on and the tempo is known),
    /// then to the nearest zero crossing (when on).
    pub(super) fn ae_snap(&self, f: usize, mods: Modifiers) -> usize {
        let ae = &self.audio_edit;
        let Some(doc) = ae.doc.as_ref() else { return f };
        // Alt disables snapping for one gesture, as in the piano roll.
        if mods.alt {
            return f.min(doc.len());
        }
        let sr = doc.sample.sample_rate.max(1.0);
        let mut f = f.min(doc.len());
        if ae.snap_beat
            && let Some(bpm) = doc.bpm.filter(|b| *b > 0.0)
        {
            let beat = 60.0 / bpm as f64 * sr as f64;
            let sub = beat / 4.0;
            f = ((f as f64 / sub).round() * sub).round().min(doc.len() as f64) as usize;
        }
        if ae.snap_zero {
            let radius = (ZERO_SNAP * sr) as usize;
            let l = edit::nearest_zero_crossing(&doc.sample.left, f, radius);
            f = if doc.sample.left == doc.sample.right || l != f { l } else { edit::nearest_zero_crossing(&doc.sample.right, f, radius) };
        }
        f
    }

    pub(super) fn ae_select(&mut self, a: usize, b: usize) {
        if let Some(doc) = self.audio_edit.doc.as_mut() {
            let (a, b) = (a.min(b), a.max(b).min(doc.len()));
            doc.sel = (b > a).then_some((a, b));
            doc.cursor = a;
        }
    }

    pub(super) fn ae_zoom_all(&mut self) {
        if let Some(doc) = self.audio_edit.doc.as_ref() {
            self.audio_edit.view = (0.0, doc.len().max(1) as f64);
        }
    }

    pub(super) fn ae_zoom_selection(&mut self) {
        let Some((a, b)) = self.audio_edit.doc.as_ref().and_then(|d| d.sel) else { return };
        let len = (b - a) as f64;
        let pad = len * 0.05;
        self.audio_edit.view = ((a as f64 - pad).max(0.0), (len + 2.0 * pad).max(MIN_VIEW));
        self.ae_refresh();
    }

    /// Zooms by `factor` (< 1: in) around frame `at`.
    pub(super) fn ae_zoom(&mut self, factor: f64, at: f64) {
        let Some(len) = self.audio_edit.doc.as_ref().map(|d| d.len().max(1) as f64) else { return };
        let (vs, vl) = self.audio_edit.view;
        let nl = (vl * factor).clamp(MIN_VIEW.min(len), len);
        let t = ((at - vs) / vl).clamp(0.0, 1.0);
        let ns = (at - t * nl).clamp(0.0, (len - nl).max(0.0));
        self.audio_edit.view = (ns, nl);
    }

    pub(super) fn ae_scroll(&mut self, frames: f64) {
        let Some(len) = self.audio_edit.doc.as_ref().map(|d| d.len().max(1) as f64) else { return };
        let (vs, vl) = self.audio_edit.view;
        self.audio_edit.view.0 = (vs + frames).clamp(0.0, (len - vl).max(0.0));
    }

    // --- Edits ---

    pub(super) fn ae_copy(&mut self) -> bool {
        let Some(doc) = self.audio_edit.doc.as_ref() else { return false };
        let Some((a, b)) = doc.sel else {
            self.ae_note(tr("Select something to copy first"));
            return false;
        };
        let s = &doc.sample;
        self.audio_edit.clipboard = Some((s.left[a..b].to_vec(), s.right[a..b].to_vec()));
        let secs = (b - a) as f32 / s.sample_rate.max(1.0);
        self.ae_note(&tr_fmt!("Copied {}", fmt_secs(secs)));
        true
    }

    pub(super) fn ae_cut(&mut self) {
        if self.ae_copy() {
            self.ae_edit(tr("Cut"), |doc, _| {
                let (a, b) = doc.sel?;
                doc.splice(a, b, Vec::new(), Vec::new());
                Some(tr("Cut to the clipboard").into())
            });
        }
    }

    pub(super) fn ae_delete(&mut self) {
        self.ae_edit(tr("Delete"), |doc, _| {
            let (a, b) = doc.sel?;
            if b - a >= doc.len() {
                return None;
            }
            doc.splice(a, b, Vec::new(), Vec::new());
            Some(tr("Deleted").into())
        });
    }

    /// Paste: replaces the selection, or goes in at the cursor.
    pub(super) fn ae_paste(&mut self) {
        let Some((l, r)) = self.audio_edit.clipboard.clone() else {
            self.ae_note(tr("Nothing to paste: copy or cut something first"));
            return;
        };
        self.ae_edit(tr("Paste"), |doc, _| {
            let (a, b) = doc.sel.unwrap_or((doc.cursor, doc.cursor));
            doc.splice(a, b, l, r);
            Some(tr("Pasted").into())
        });
    }

    /// Keeps only the selection.
    pub(super) fn ae_crop(&mut self) {
        self.ae_edit(tr("Crop"), |doc, _| {
            let (a, b) = doc.sel?;
            let len = doc.len();
            doc.splice(b, len, Vec::new(), Vec::new());
            doc.splice(0, a, Vec::new(), Vec::new());
            doc.sel = None;
            Some(tr("Cropped to the selection").into())
        });
        self.ae_zoom_all();
    }

    /// Inserts silence at the cursor (or replaces the selection with silence
    /// of the same length).
    pub(super) fn ae_insert_silence(&mut self) {
        self.ae_edit(tr("Insert silence"), |doc, ae| {
            let sr = doc.sample.sample_rate.max(1.0);
            let (a, b, n) = match doc.sel {
                Some((a, b)) => (a, b, b - a),
                None => (doc.cursor, doc.cursor, (ae.silence_ms * 0.001 * sr) as usize),
            };
            doc.splice(a, b, vec![0.0; n], vec![0.0; n]);
            Some(tr_fmt!("{} of silence", fmt_secs(n as f32 / sr)))
        });
    }

    pub(super) fn ae_select_all(&mut self) {
        let len = self.audio_edit.doc.as_ref().map_or(0, Doc::len);
        self.ae_select(0, len);
    }

    // --- Processing (selection, or everything) ---

    pub(super) fn ae_gain(&mut self) {
        let g = self.audio_edit.gain_db;
        self.ae_edit(tr("Gain"), |doc, _| {
            if g.abs() < 0.01 {
                return None;
            }
            let (a, b) = doc.range();
            doc.process(a, b, |x| edit::apply_gain(x, ponzik_dsp::util::db_to_gain(g)));
            Some(tr_fmt!("Gain {} dB", signed(g, 1)))
        });
    }

    pub(super) fn ae_normalize(&mut self) {
        let (mode, peak_t, lufs_t) = (self.audio_edit.norm, self.audio_edit.peak_target, self.audio_edit.lufs_target);
        let mut why = None;
        self.ae_edit(tr("Normalize"), |doc, _| {
            let (a, b) = doc.range();
            let s = &doc.sample;
            let (l, r) = (&s.left[a..b], &s.right[a..b]);
            let g = match mode {
                NormMode::Peak => edit::peak_normalize_gain(&[l, r], peak_t),
                NormMode::Lufs => edit::lufs_normalize_gain(l, r, s.sample_rate, lufs_t),
            };
            let Some(g) = g else {
                why = Some(match mode {
                    NormMode::Peak => tr("Nothing to normalize: the sound is silent"),
                    NormMode::Lufs => tr("Too short or too quiet to measure its loudness (needs 0.4 s)"),
                });
                return None;
            };
            doc.process(a, b, |x| edit::apply_gain(x, g));
            let db = 20.0 * g.log10();
            Some(match mode {
                NormMode::Peak => tr_fmt!("Normalized to {} dBFS ({} dB)", signed(peak_t, 1), signed(db, 1)),
                NormMode::Lufs => tr_fmt!("Normalized to {} LUFS ({} dB)", signed(lufs_t, 1), signed(db, 1)),
            })
        });
        if let Some(w) = why {
            self.ae_note(w);
        }
    }

    pub(super) fn ae_fade(&mut self, fade_in: bool) {
        let shape = self.audio_edit.fade;
        let label = if fade_in { tr("Fade in") } else { tr("Fade out") };
        self.ae_edit(label, |doc, _| {
            let (a, b) = doc.sel?;
            doc.process(a, b, |x| if fade_in { edit::fade_in(x, shape) } else { edit::fade_out(x, shape) });
            Some(label.into())
        });
        if self.audio_edit.doc.as_ref().is_some_and(|d| d.sel.is_none()) {
            self.ae_note(tr("Select where the fade goes first"));
        }
    }

    pub(super) fn ae_reverse(&mut self) {
        self.ae_edit(tr("Reverse"), |doc, _| {
            let (a, b) = doc.range();
            doc.process(a, b, |x| x.reverse());
            Some(tr("Reversed").into())
        });
    }

    pub(super) fn ae_dc(&mut self) {
        self.ae_edit(tr("Remove DC offset"), |doc, _| {
            let (a, b) = doc.range();
            let ramp = if (a, b) == (0, doc.len()) { 0 } else { (doc.sample.sample_rate * 0.005) as usize };
            let mut off = 0.0f32;
            doc.process(a, b, |x| off = off.max(edit::remove_dc(x, ramp).abs()));
            Some(tr_fmt!("DC offset removed ({}%)", fr(off * 100.0, 2)))
        });
    }

    /// Learns the noise from the selection.
    pub(super) fn ae_learn_noise(&mut self) {
        let Some(doc) = self.audio_edit.doc.as_ref() else { return };
        let Some((a, b)) = doc.sel else {
            self.ae_note(tr("Select a stretch of noise alone first (no voice, no music)"));
            return;
        };
        let s = &doc.sample;
        match NoiseProfile::learn_channels(&[&s.left[a..b], &s.right[a..b]], s.sample_rate) {
            Some(p) => {
                self.audio_edit.noise = Some(p);
                self.ae_note(&tr_fmt!("Noise learnt from {}: now select what to clean, or nothing for everything", fmt_secs((b - a) as f32 / s.sample_rate)));
            }
            None => self.ae_note(&tr_fmt!("Select at least {} of noise", fmt_secs(denoise::FFT as f32 / s.sample_rate))),
        }
    }

    pub(super) fn ae_denoise(&mut self) {
        let Some(profile) = self.audio_edit.noise.clone() else {
            self.ae_note(tr("Learn the noise first: select noise alone, then Learn"));
            return;
        };
        let settings = self.audio_edit.denoise;
        self.ae_edit(tr("Reduce noise"), |doc, _| {
            let (a, b) = doc.range();
            let s = &doc.sample;
            // Context around the range, so the edges are processed like the rest.
            let pad = denoise::FFT;
            let (pa, pb) = (a.saturating_sub(pad), (b + pad).min(s.len()));
            let l = denoise::denoise(&s.left[pa..pb], &profile, settings);
            let r = if s.left == s.right { l.clone() } else { denoise::denoise(&s.right[pa..pb], &profile, settings) };
            let (l, r) = (l[a - pa..b - pa].to_vec(), r[a - pa..b - pa].to_vec());
            doc.splice(a, b, l, r);
            if (a, b) == (0, doc.len()) {
                doc.sel = None;
            }
            Some(tr_fmt!("Noise lowered by up to {} dB", fr(settings.reduction_db, 0)))
        });
    }

    pub(super) fn ae_declick(&mut self) {
        let sens = self.audio_edit.declick;
        let mut none = false;
        self.ae_edit(tr("Declick"), |doc, _| {
            let (a, b) = doc.range();
            let sr = doc.sample.sample_rate;
            let mut n = 0;
            doc.process(a, b, |x| n += edit::declick(x, sr, sens));
            none = n == 0;
            (n > 0).then(|| tr_fmt!("{} clicks repaired", n))
        });
        if none {
            self.ae_note(tr("No click found (raise the sensitivity to catch smaller ones)"));
        }
    }

    pub(super) fn ae_declip(&mut self) {
        let mut none = false;
        self.ae_edit(tr("Declip"), |doc, _| {
            let (a, b) = doc.range();
            let mut n = 0;
            doc.process(a, b, |x| n += edit::declip(x));
            none = n == 0;
            if n == 0 {
                return None;
            }
            // Rebuilt peaks may pass full scale: bring them back under it.
            let s = &doc.sample;
            if let Some(g) = edit::peak_normalize_gain(&[&s.left[a..b], &s.right[a..b]], -0.3).filter(|&g| g < 1.0) {
                doc.process(a, b, |x| edit::apply_gain(x, g));
            }
            Some(tr_fmt!("{} clipped peaks rebuilt", n))
        });
        if none {
            self.ae_note(tr("No clipping found"));
        }
    }

    pub(super) fn ae_trim_silence(&mut self) {
        let thr = self.audio_edit.trim_db;
        let mut why = None;
        self.ae_edit(tr("Trim silence"), |doc, _| {
            let s = &doc.sample;
            let pad = (s.sample_rate * 0.005) as usize;
            let Some((a, b)) = edit::silence_bounds(&s.left, &s.right, thr, pad) else {
                why = Some(tr("Everything is below the threshold"));
                return None;
            };
            if a == 0 && b == doc.len() {
                why = Some(tr("No silence at the edges"));
                return None;
            }
            let cut = a + (doc.len() - b);
            let len = doc.len();
            doc.splice(b, len, Vec::new(), Vec::new());
            doc.splice(0, a, Vec::new(), Vec::new());
            // A short fade at the end: the cut never clicks.
            let n = doc.len();
            let fade = ((doc.sample.sample_rate * 0.01) as usize).min(n);
            doc.process(n - fade, n, |x| edit::fade_out(x, FadeShape::Linear));
            doc.sel = None;
            Some(tr_fmt!("Trimmed {} of silence", fmt_secs(cut as f32 / doc.sample.sample_rate)))
        });
        if let Some(w) = why {
            self.ae_note(w);
        }
        self.ae_zoom_all();
    }

    /// Stretches the range by `factor` (time), keeping its pitch.
    pub(super) fn ae_stretch(&mut self, factor: f64) {
        let algo = self.audio_edit.algo;
        self.ae_edit(tr("Time stretch"), |doc, _| {
            if (factor - 1.0).abs() < 1e-4 {
                return None;
            }
            let (a, b) = doc.range();
            let s = &doc.sample;
            let whole = (a, b) == (0, doc.len());
            let (l, r) = edit::stretch(&s.left[a..b], &s.right[a..b], s.sample_rate, factor, algo);
            doc.splice(a, b, l, r);
            if whole {
                doc.sel = None;
                doc.bpm = doc.bpm.map(|bpm| bpm / factor as f32);
            }
            Some(tr_fmt!("Stretched to {}%", fr(factor as f32 * 100.0, 1)))
        });
    }

    pub(super) fn ae_pitch(&mut self) {
        let (st, algo) = (self.audio_edit.semitones, self.audio_edit.algo);
        self.ae_edit(tr("Pitch shift"), |doc, _| {
            if st.abs() < 0.01 {
                return None;
            }
            let (a, b) = doc.range();
            let s = &doc.sample;
            let whole = (a, b) == (0, doc.len());
            let (l, r) = edit::pitch_shift(&s.left[a..b], &s.right[a..b], s.sample_rate, st, algo);
            doc.splice(a, b, l, r);
            if whole {
                doc.sel = None;
            }
            Some(tr_fmt!("Pitch shifted {} semitones", signed(st, 1)))
        });
    }

    /// Fits the sound (or selection) to the project tempo, from its own.
    pub(super) fn ae_match_tempo(&mut self) {
        let Some(from) = self.audio_edit.doc.as_ref().and_then(|d| d.bpm) else {
            self.ae_note(tr("The tempo of the sound is unknown: detect it first"));
            return;
        };
        let to = self.project.bpm;
        if (from - to).abs() < 0.01 {
            self.ae_note(tr("Already at the project tempo"));
            return;
        }
        self.ae_stretch(from as f64 / to as f64);
        if let Some(doc) = self.audio_edit.doc.as_mut() {
            doc.bpm = Some(to);
        }
        self.ae_note(&tr_fmt!("{} → {} BPM", fr(from, 2), fr(to, 2)));
    }

    /// Makes the selection a seamless loop and keeps only the loop.
    pub(super) fn ae_loop(&mut self) {
        let ms = self.audio_edit.xfade_ms;
        self.ae_edit(tr("Make a seamless loop"), |doc, _| {
            let (a, b) = doc.sel?;
            let s = &doc.sample;
            let n = (ms * 0.001 * s.sample_rate) as usize;
            let l = edit::crossfade_loop(&s.left, a, b, n);
            let r = edit::crossfade_loop(&s.right, a, b, n);
            let len = doc.len();
            doc.splice(b, len, Vec::new(), Vec::new());
            doc.splice(a, b, l, r);
            doc.splice(0, a, Vec::new(), Vec::new());
            doc.sel = None;
            Some(tr("Seamless loop: the end flows into the start").into())
        });
        if self.audio_edit.doc.as_ref().is_some_and(|d| d.sel.is_none()) && !self.audio_edit.looping {
            self.audio_edit.looping = true;
        }
        self.ae_zoom_all();
    }

    /// Converts the whole sound to the chosen sample rate.
    pub(super) fn ae_resample(&mut self) {
        let to = self.audio_edit.rate as f32;
        self.ae_edit(tr("Convert the sample rate"), |doc, _| {
            let s = &doc.sample;
            let from = s.sample_rate;
            if (from - to).abs() < 0.5 {
                return None;
            }
            let ratio = to as f64 / from as f64;
            let new = Sample { left: edit::resample(&s.left, from, to), right: edit::resample(&s.right, from, to), sample_rate: to, ..(**s).clone() };
            let len = doc.len();
            let n = new.len();
            doc.sample = Arc::new(new);
            doc.remap(0, len, n);
            doc.sel = doc.sel.map(|(a, b)| ((a as f64 * ratio) as usize, (b as f64 * ratio) as usize));
            Some(tr_fmt!("Converted from {} Hz to {} Hz", fr(from, 0), fr(to, 0)))
        });
        self.ae_zoom_all();
    }

    /// Both channels become their mix.
    pub(super) fn ae_to_mono(&mut self) {
        self.ae_edit(tr("Make mono"), |doc, _| {
            let s = &doc.sample;
            if s.left == s.right {
                return None;
            }
            let m = onset::mono(&s.left, &s.right);
            doc.sample = Arc::new(Sample { left: m.clone(), right: m, ..(**s).clone() });
            Some(tr("Mono: both channels are their mix").into())
        });
    }

    // --- Markers and regions ---

    pub(super) fn ae_add_marker(&mut self) {
        self.ae_edit(tr("Add a marker"), |doc, _| {
            let pos = doc.sel.map_or(doc.cursor, |s| s.0);
            let name = tr_fmt!("Marker {}", doc.markers.len() + 1);
            doc.markers.push(Marker { pos, name: name.clone() });
            doc.markers.sort_by_key(|m| m.pos);
            Some(name)
        });
    }

    pub(super) fn ae_add_region(&mut self) {
        if self.audio_edit.doc.as_ref().is_some_and(|d| d.sel.is_none()) {
            self.ae_note(tr("Select the span of the region first"));
            return;
        }
        self.ae_edit(tr("Add a region"), |doc, _| {
            let (a, b) = doc.sel?;
            let name = tr_fmt!("Region {}", doc.regions.len() + 1);
            doc.regions.push(Region { start: a, end: b, name: name.clone() });
            doc.regions.sort_by_key(|r| r.start);
            Some(name)
        });
    }

    // --- Slicing ---

    /// Detects the transients with the current sensitivity and gap.
    pub(super) fn ae_detect_slices(&mut self) {
        let (sens, gap) = (self.audio_edit.sensitivity, self.audio_edit.min_gap_ms * 0.001);
        let Some(o) = self.ae_onsets() else { return };
        // A hit right at the start is the first slice itself.
        let head = (gap * o.analysis.sample_rate) as usize;
        let found: Vec<usize> = o.analysis.pick_with(&o.mono, sens, gap, MAX_SLICES).iter().map(|o| o.pos).filter(|&p| p > head).collect();
        let n = found.len();
        self.ae_edit(tr("Detect slices"), |doc, _| {
            doc.slices = found;
            Some(tr_fmt!("{} slices", doc.slices.len() + 1))
        });
        if n == 0 {
            self.ae_note(tr("One slice: no hit found (raise the sensitivity)"));
        }
    }

    /// Adds a slice cut (snapped), or moves an existing one.
    pub(super) fn ae_add_slice(&mut self, at: usize) {
        self.ae_edit(tr("Add a slice"), |doc, _| {
            if at == 0 || at >= doc.len() || doc.slices.contains(&at) || doc.slices.len() + 1 >= MAX_SLICES {
                return None;
            }
            doc.slices.push(at);
            doc.slices.sort_unstable();
            Some(tr_fmt!("{} slices", doc.slices.len() + 1))
        });
    }

    pub(super) fn ae_remove_slice(&mut self, k: usize) {
        self.ae_edit(tr("Remove a slice"), |doc, _| {
            (k < doc.slices.len()).then(|| {
                doc.slices.remove(k);
                tr_fmt!("{} slices", doc.slices.len() + 1)
            })
        });
    }

    pub(super) fn ae_clear_slices(&mut self) {
        self.ae_edit(tr("Clear slices"), |doc, _| {
            (!doc.slices.is_empty()).then(|| {
                doc.slices.clear();
                tr("Slices cleared").into()
            })
        });
    }

    /// Detects the tempo; `say` shows the result.
    pub(super) fn ae_detect_tempo(&mut self, say: bool) {
        let Some(dur) = self.audio_edit.doc.as_ref().map(|d| d.sample.duration()) else { return };
        let est = self.ae_onsets().and_then(|o| tempo::estimate(&o.analysis, dur));
        self.audio_edit.tempo = est;
        // On opening, only a clear beat sets the tempo (a voice has none);
        // asked for, the estimate is taken whatever its confidence.
        let keep = est.filter(|e| say || e.confidence >= 0.45);
        if let Some(doc) = self.audio_edit.doc.as_mut() {
            doc.bpm = keep.map(|e| e.bpm).or(doc.bpm);
        }
        if say {
            match est {
                Some(e) => self.ae_note(&match e.alternative {
                    Some(alt) => tr_fmt!("Tempo: {} BPM (or {})", fr(e.bpm, 2), fr(alt, 2)),
                    None => tr_fmt!("Tempo: {} BPM", fr(e.bpm, 2)),
                }),
                None => self.ae_note(tr("No clear beat found")),
            }
        }
    }

    /// Takes the half or double tempo.
    pub(super) fn ae_use_alternative(&mut self) {
        let Some(est) = self.audio_edit.tempo else { return };
        let Some(alt) = est.alternative else { return };
        self.audio_edit.tempo = Some(TempoEstimate { bpm: alt, alternative: Some(est.bpm), ..est });
        if let Some(doc) = self.audio_edit.doc.as_mut() {
            doc.bpm = Some(alt);
        }
        self.ae_note(&tr_fmt!("Tempo: {} BPM", fr(alt, 2)));
    }

    // --- Out of the editor ---

    /// Writes the sound (WAV or FLAC) next to the project; returns its path.
    pub(super) fn ae_write(&mut self, sample: Sample, stem: &str, flac: bool) -> Option<String> {
        let dir = audio_dir(self);
        if let Err(e) = std::fs::create_dir_all(&dir) {
            self.toast(&format!("{} : {e}", dir.display()));
            return None;
        }
        let stem = super::compat::file_name(stem);
        let path = unique_path(&dir, &stem, if flac { "flac" } else { "wav" });
        let written = if flac { std::fs::write(&path, encode_flac(&sample)) } else { ponzik_engine::wav::write_sample(&path, &sample) };
        if let Err(e) = written {
            self.toast(&format!("{} : {e}", path.display()));
            return None;
        }
        let key = path.to_string_lossy().into_owned();
        let name = path.file_stem().map_or(String::new(), |s| s.to_string_lossy().into_owned());
        self.samples.insert(key.clone(), Arc::new(Sample { name, ..sample }));
        if !self.sample_files.contains(&path) {
            self.sample_files.push(path);
            self.sample_files.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()));
        }
        Some(key)
    }

    fn ae_base(&self) -> String {
        let n = self.audio_edit.name.trim();
        let n = n.strip_suffix(" edit").unwrap_or(n);
        if n.is_empty() { tr("Audio").into() } else { n.to_string() }
    }

    /// Saves the whole edit as a new file.
    pub(super) fn ae_save(&mut self, flac: bool) -> Option<String> {
        let s = self.audio_edit.doc.as_ref()?.export();
        let stem = format!("{} edit", self.ae_base());
        let path = self.ae_write(s, &stem, flac)?;
        let short = Path::new(&path).file_name().map_or(String::new(), |s| s.to_string_lossy().into_owned());
        self.ae_note(&tr_fmt!("Saved as {}", short));
        self.audio_edit.name = Path::new(&path).file_stem().map_or(stem, |s| s.to_string_lossy().into_owned());
        self.audio_edit.path = Some(path.clone());
        Some(path)
    }

    /// A new Sampler channel plays the edit.
    pub(super) fn ae_to_sampler(&mut self) {
        if let Some(path) = self.ae_save(false) {
            self.add_sample_channel(&path);
        }
    }

    pub(super) fn ae_to_playlist(&mut self) {
        if let Some(path) = self.ae_save(false) {
            self.add_audio_clip(&path, None);
        }
    }

    /// The channel or clip the sound came from plays the edit (undoable in the project).
    pub(super) fn ae_replace_source(&mut self) {
        let source = self.audio_edit.source;
        let ok = match source {
            Source::Channel(ch) => ch < self.project.channels.len(),
            Source::Clip(i) => i < self.project.audio_clips.len(),
            Source::Browser => false,
        };
        if !ok {
            self.ae_note(tr("This sound came from no channel or clip: use Save or Send instead"));
            return;
        }
        let Some(path) = self.ae_save(false) else { return };
        match source {
            Source::Channel(ch) => {
                let has_slices = self.audio_edit.doc.as_ref().is_some_and(|d| !d.slices.is_empty());
                let inst = &mut self.project.channels[ch].instrument;
                inst.sample = Some(path);
                if inst.kind == "slicer" && has_slices {
                    inst.set(SLICER_MODE, SLICER_MARKERS);
                }
                self.commit(tr("Audio editor: replace sample"));
                let name = self.project.channels[ch].name.clone();
                self.toast(&tr_fmt!("\"{}\" now plays the edited sound · Ctrl+Z brings the old one back", name));
            }
            Source::Clip(i) => {
                let secs = self.samples.get(&path).map_or(0.0, |s| s.duration() as f64);
                let ticks = (secs * self.project.bpm as f64 / 60.0 * PPQ as f64).round().max(24.0) as u32;
                let clip = &mut self.project.audio_clips[i];
                clip.path = path;
                clip.offset = 0.0;
                if !clip.warp {
                    clip.len = ticks;
                }
                self.commit(tr("Audio editor: replace clip"));
                self.toast(tr("The playlist clip now plays the edited sound · Ctrl+Z brings the old one back"));
            }
            Source::Browser => {}
        }
    }

    /// Slice `k` of the edit, with a 3 ms fade out so it never clicks.
    fn ae_slice_sample(&self, k: usize) -> Option<Sample> {
        let doc = self.audio_edit.doc.as_ref()?;
        let b = doc.bounds();
        let (s, e) = (*b.get(k)?, *b.get(k + 1)?);
        let src = &doc.sample;
        let (mut l, mut r) = (src.left[s..e].to_vec(), src.right[s..e].to_vec());
        let fade = ((src.sample_rate * 0.003) as usize).min(l.len());
        let n = l.len();
        edit::fade_out(&mut l[n - fade..], FadeShape::Linear);
        edit::fade_out(&mut r[n - fade..], FadeShape::Linear);
        Some(Sample { name: String::new(), sample_rate: src.sample_rate, left: l, right: r, markers: Vec::new() })
    }

    /// Each slice in its own file; returns their paths.
    pub(super) fn ae_export_slices(&mut self) -> Vec<String> {
        let n = self.audio_edit.doc.as_ref().map_or(0, |d| d.bounds().len() - 1);
        let base = self.ae_base();
        let paths: Vec<String> = (0..n)
            .filter_map(|k| {
                let s = self.ae_slice_sample(k)?;
                self.ae_write(s, &tr_fmt!("{} slice {}", base, k + 1), false)
            })
            .collect();
        if !paths.is_empty() {
            self.ae_note(&tr_fmt!("{} slices saved as files, next to the project", paths.len()));
        }
        paths
    }

    /// Note start (ticks) of every slice bound, at the sound's own tempo
    /// (the groove is kept) or the project's.
    fn ae_slice_ticks(&self) -> Vec<u32> {
        let Some(doc) = self.audio_edit.doc.as_ref() else { return Vec::new() };
        let sr = doc.sample.sample_rate.max(1.0) as f64;
        let bpm = doc.bpm.unwrap_or(self.project.bpm) as f64;
        doc.bounds().iter().map(|&f| (f as f64 / sr * bpm / 60.0 * PPQ as f64).round() as u32).collect()
    }

    /// A new pattern holding `notes` for channel `ch`, `end` ticks long.
    fn ae_pattern(&mut self, ch: usize, name: String, notes: Vec<Note>, end: u32) -> usize {
        let p = self.project.add_pattern();
        let pat = &mut self.project.patterns[p];
        pat.name = name;
        pat.len = end.div_ceil(TICKS_PER_BAR).max(1) * TICKS_PER_BAR;
        if pat.notes.len() <= ch {
            pat.notes.resize(ch + 1, Vec::new());
        }
        pat.notes[ch] = notes;
        self.select_pattern(p);
        p
    }

    /// The slices go to a new Slicer (notes 60 + k); with `midi`, a pattern
    /// replays them in their original rhythm.
    pub(super) fn ae_to_slicer(&mut self, midi: bool) {
        let Some(path) = self.ae_save(false) else { return };
        let base = self.ae_base();
        let mut def = InstrumentDef::new("slicer").with(SLICER_MODE, SLICER_MARKERS);
        def.sample = Some(path);
        let ch = self.project.add_channel(tr_fmt!("{} slices", base), def, 60);
        self.focus = Focus::Channel(ch);
        let n = self.audio_edit.doc.as_ref().map_or(1, |d| d.bounds().len() - 1);
        if midi {
            let t = self.ae_slice_ticks();
            let notes = (0..n).map(|k| Note { start: t[k], len: (t[k + 1] - t[k]).max(1), key: 60 + k as u8, vel: 0.8, ..Note::DEFAULT }).collect();
            self.ae_pattern(ch, tr_fmt!("{} MIDI", base), notes, t[n]);
            self.commit(tr("Slice to MIDI"));
            self.toast(&tr_fmt!("Slicer and pattern ready: {} slices on the keys from C5, in their original rhythm", n));
        } else {
            self.commit(tr("Slices to the Slicer"));
            self.toast(&tr_fmt!("{} slices in a new Slicer, on the keys from C5", n));
        }
    }

    /// One pad per slice (16 at most) in a new Drum Pads channel.
    pub(super) fn ae_to_pads(&mut self) {
        let n = self.audio_edit.doc.as_ref().map_or(0, |d| d.bounds().len() - 1).min(PADS);
        let base = self.ae_base();
        let mut def = InstrumentDef::new("drum_pads");
        def.pads = vec![Vec::new(); PADS];
        let mut placed = 0;
        for k in 0..n {
            let Some(s) = self.ae_slice_sample(k) else { continue };
            if let Some(path) = self.ae_write(s, &tr_fmt!("{} pad {}", base, k + 1), false) {
                def.pads[k] = vec![path];
                placed += 1;
            }
        }
        if placed == 0 {
            return;
        }
        let ch = self.project.add_channel(tr_fmt!("{} kit", base), def, 60);
        self.focus = Focus::Channel(ch);
        self.commit(tr("Slices to Drum Pads"));
        self.toast(&tr_fmt!("{} pads filled, one slice each", placed));
    }

    /// "Make a drum pattern from a break": every slice is told kick, snare or
    /// hat; the loudest of each kind becomes a pad, and a pattern replays the
    /// break's rhythm on those three pads.
    pub(super) fn ae_drum_pattern(&mut self) {
        let Some(doc) = self.audio_edit.doc.clone() else { return };
        let b = doc.bounds();
        let n = b.len() - 1;
        let sr = doc.sample.sample_rate;
        let mono = onset::mono(&doc.sample.left, &doc.sample.right);
        let kinds: Vec<(Hit, f32)> = (0..n).map(|k| (classify(&mono[b[k]..b[k + 1]], sr), edit::peak(&mono[b[k]..b[k + 1]]))).collect();
        let base = self.ae_base();
        let mut def = InstrumentDef::new("drum_pads");
        def.pads = vec![Vec::new(); PADS];
        let mut used = Vec::new();
        for hit in [Hit::Kick, Hit::Snare, Hit::Hat] {
            let best = (0..n).filter(|&k| kinds[k].0 == hit).max_by(|&x, &y| kinds[x].1.total_cmp(&kinds[y].1));
            let Some(k) = best else { continue };
            let Some(s) = self.ae_slice_sample(k) else { continue };
            let label = match hit {
                Hit::Kick => tr("kick"),
                Hit::Snare => tr("snare"),
                Hit::Hat => tr("hat"),
            };
            if let Some(path) = self.ae_write(s, &format!("{base} {label}"), false) {
                def.pads[(hit.note() - 36) as usize] = vec![path];
                used.push(hit);
            }
        }
        if used.is_empty() {
            return;
        }
        let ch = self.project.add_channel(tr_fmt!("{} drums", base), def, 60);
        self.focus = Focus::Channel(ch);
        let t = self.ae_slice_ticks();
        let loudest = kinds.iter().fold(1e-6f32, |m, k| m.max(k.1));
        let notes: Vec<Note> = (0..n)
            .filter(|&k| used.contains(&kinds[k].0))
            .map(|k| Note {
                start: t[k],
                len: (t[k + 1] - t[k]).clamp(1, PPQ / 4),
                key: kinds[k].0.note(),
                vel: (0.35 + 0.65 * kinds[k].1 / loudest).min(1.0),
                ..Note::DEFAULT
            })
            .collect();
        let count = |h: Hit| kinds.iter().filter(|k| k.0 == h).count();
        let (kicks, snares, hats) = (count(Hit::Kick), count(Hit::Snare), count(Hit::Hat));
        self.ae_pattern(ch, tr_fmt!("{} beat", base), notes, t[n]);
        self.commit(tr("Drum pattern from a break"));
        self.toast(&tr_fmt!("Drum pattern rebuilt: {} kicks, {} snares, {} hats on Drum Pads", kicks, snares, hats));
    }

    // --- Free recording ---

    pub(super) fn ae_toggle_record(&mut self) {
        if self.audio_edit.capture.is_some() {
            self.send(Command::Capture(None));
            return;
        }
        if !self.open_input() {
            self.toast(tr("No microphone: check the input in the recording panel"));
            return;
        }
        self.ae_stop();
        let frames = (REC_SECONDS * self.sample_rate as f32) as usize;
        self.send(Command::Capture(Some(Box::new(RecordBuffer::new(frames, self.sample_rate as f32)))));
        self.audio_edit.capture = Some(Capture { started: self.time, levels: Vec::new() });
    }

    /// Follows a recording: its level as it grows, then the take when it comes back.
    pub(super) fn ae_poll_capture(&mut self) {
        if self.audio_edit.capture.is_none() {
            return;
        }
        let level = self.rec.input_level();
        if let Some(cap) = self.audio_edit.capture.as_mut() {
            cap.levels.push(level);
        }
        let Some(buf) = self.engine.take_capture() else { return };
        self.audio_edit.capture = None;
        if buf.len == 0 {
            self.toast(tr("Nothing was recorded: no input signal"));
            return;
        }
        let source = self.rec.settings.source;
        self.audio_edit.takes += 1;
        let name = tr_fmt!("Recording {}", self.audio_edit.takes);
        let mut s = buf.into_sample(name.clone());
        match source {
            InputSource::Left => s.right = s.left.clone(),
            InputSource::Right => s.left = s.right.clone(),
            InputSource::Stereo | InputSource::Resample(_) => {}
        }
        self.open_sample_in_editor(Arc::new(s), None, Source::Browser);
        self.audio_edit.name = name;
        self.ae_note(tr("Recorded: trim it, clean it, then save or send it"));
    }

    // --- Keyboard ---

    /// The editor's keys (Edison's where they exist); true when taken.
    pub fn ae_key(&mut self, key: Key, mods: Modifiers) -> bool {
        if self.audio_edit.doc.is_none() {
            if key == Key::Char('r') && !mods.ctrl {
                self.ae_toggle_record();
                return true;
            }
            return false;
        }
        let ctrl = mods.ctrl;
        match key {
            Key::Space if !ctrl => self.ae_toggle_play(),
            Key::Char('z') if ctrl => return self.ae_undo(mods.shift),
            Key::Char('y') if ctrl => return self.ae_undo(true),
            Key::Char('a') if ctrl => self.ae_select_all(),
            Key::Char('c') if ctrl => {
                self.ae_copy();
            }
            Key::Char('x') if ctrl => self.ae_cut(),
            Key::Char('v') if ctrl => self.ae_paste(),
            Key::Delete if ctrl => self.ae_crop(),
            Key::Delete | Key::Backspace => self.ae_delete(),
            Key::Char('n') if ctrl => self.ae_normalize(),
            Key::Char('r') if ctrl => self.ae_reverse(),
            Key::Char('r') => self.ae_toggle_record(),
            Key::Char('i') if ctrl && mods.shift => self.ae_fade(true),
            Key::Char('i') if ctrl => self.ae_insert_silence(),
            Key::Char('o') if ctrl && mods.shift => self.ae_fade(false),
            Key::Char('l') if !ctrl => self.audio_edit.looping = !self.audio_edit.looping,
            Key::Char('m') if !ctrl && mods.shift => self.ae_add_region(),
            Key::Char('m') if !ctrl => self.ae_add_marker(),
            Key::Char('s') if !ctrl && mods.shift => self.audio_edit.spectrum = !self.audio_edit.spectrum,
            Key::Char('z') => self.ae_zoom_selection(),
            Key::Home => self.ae_zoom_all(),
            Key::Escape => {
                self.ae_stop();
                if let Some(doc) = self.audio_edit.doc.as_mut() {
                    doc.sel = None;
                }
            }
            Key::Left | Key::Right if !ctrl => {
                let (vs, vl) = self.audio_edit.view;
                let step = vl * 0.1 * if key == Key::Left { -1.0 } else { 1.0 };
                self.audio_edit.view.0 = vs;
                self.ae_scroll(step);
            }
            _ => return false,
        }
        true
    }

    /// Capture: `demo` (a break with a little hiss, its slices), `tone` (a
    /// sung note over hiss), `empty`, or a file; PONZIK_AUDIOEDIT_SPECTRUM
    /// shows the spectrogram. `,tab:<n>` picks the tool tab, `,zoom` zooms in on the
    /// selection, `,rec` shows a recording in progress.
    pub fn screenshot_audio_edit(&mut self, spec: &str) {
        let mut parts = spec.split(',');
        let what = parts.next().unwrap_or("demo").trim();
        let sr = 48_000.0;
        let sample = match what {
            "empty" => {
                self.audio_edit.active = true;
                self.set_view(View::Lab);
                return;
            }
            "tone" => demo_voice(sr),
            "demo" | "" => {
                let mut s = super::samplelab::demo_break(sr);
                // A little hiss, for the noise tools.
                let mut seed = 77u32;
                for (l, r) in s.left.iter_mut().zip(s.right.iter_mut()) {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    let n = ((seed >> 8) as f32 / (1u32 << 23) as f32 - 1.0) * 0.004;
                    *l += n;
                    *r += n * 0.8;
                }
                s
            }
            path => {
                self.open_in_editor(path, Source::Browser);
                match self.audio_edit.doc.as_ref() {
                    Some(d) => (*d.sample).clone(),
                    None => return,
                }
            }
        };
        self.open_sample_in_editor(Arc::new(sample), None, Source::Browser);
        self.audio_edit.name = if what == "tone" { "Voice take".into() } else { "Break 92".into() };
        self.ae_detect_slices();
        let len = self.audio_edit.doc.as_ref().map_or(0, Doc::len);
        let sel = (len * 5 / 16, len * 7 / 16);
        self.ae_select(sel.0, sel.1);
        if let Some(doc) = self.audio_edit.doc.as_mut() {
            doc.markers.push(Marker { pos: len / 8, name: tr_fmt!("Marker {}", 1) });
            doc.regions.push(Region { start: len * 9 / 16, end: len * 13 / 16, name: tr_fmt!("Region {}", 1) });
            doc.cursor = sel.0;
        }
        self.audio_edit.spectrum = std::env::var_os("PONZIK_AUDIOEDIT_SPECTRUM").is_some();
        for p in parts {
            let p = p.trim();
            if let Some(n) = p.strip_prefix("tab:").and_then(|n| n.parse::<usize>().ok()) {
                self.audio_edit.tab = Tab::ALL[n.min(3)];
            } else if p == "zoom" {
                self.ae_zoom_selection();
                let v = self.audio_edit.view;
                self.audio_edit.shown = v;
            } else if p == "deep" {
                // Sample-accurate: 120 frames across, on the attack of a hit.
                let hit = self.audio_edit.doc.as_ref().and_then(|d| d.slices.iter().copied().find(|&s| s > sel.0)).unwrap_or(sel.0);
                let at = hit as f64 + 40.0;
                self.audio_edit.view = (at, 200.0);
                self.audio_edit.shown = (at, 200.0);
            } else if p == "rec" {
                let levels = (0..240).map(|i| (0.45 + 0.3 * ((i as f32) * 0.21).sin() * ((i as f32) * 0.037).cos()).abs()).collect();
                self.audio_edit.capture = Some(Capture { started: self.time - 4.2, levels });
            } else if p == "noise" {
                let s = self.audio_edit.doc.as_ref().map(|d| d.sample.clone());
                if let Some(s) = s {
                    let n = (s.len() / 10).max(4_096).min(s.len());
                    self.audio_edit.noise = NoiseProfile::learn_channels(&[&s.left[..n], &s.right[..n]], s.sample_rate);
                }
            } else if p == "chop" {
                self.audio_edit.active = false;
            }
        }
        if self.audio_edit.capture.is_none() {
            self.ae_play_range(sel.0, sel.1, true);
        }
        self.audio_edit.looping = true;
        self.ae_note(&tr_fmt!("Normalized to {} dBFS ({} dB)", signed(-0.3, 1), signed(2.1, 1)));
    }
}

/// A sung-like tone over hiss, with a quiet lead-in of noise alone.
pub(super) fn demo_voice(sr: f32) -> Sample {
    let len = (sr * 4.0) as usize;
    let mut seed = 5u32;
    let tau = std::f32::consts::TAU;
    let mut phase = 0.0f32;
    let left: Vec<f32> = (0..len)
        .map(|i| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let n = ((seed >> 8) as f32 / (1u32 << 23) as f32 - 1.0) * 0.03;
            let t = i as f32 / sr;
            let on = ((t - 0.6) * 8.0).clamp(0.0, 1.0) * ((3.7 - t) * 6.0).clamp(0.0, 1.0);
            let f = 220.0 * (1.0 + 0.012 * (t * 5.5 * tau).sin()) * if t > 2.1 { 1.335 } else { 1.0 };
            phase += f / sr;
            let v = (phase * tau).sin() * 0.5 + (2.0 * phase * tau).sin() * 0.22 + (3.0 * phase * tau).sin() * 0.12 + (5.0 * phase * tau).sin() * 0.05;
            v * 0.6 * on + n
        })
        .collect();
    Sample { name: "Voice take".into(), sample_rate: sr, right: left.clone(), left, markers: Vec::new() }
}

/// Number with the locale's decimal separator.
pub(super) fn fr(x: f32, decimals: usize) -> String {
    ponzik_i18n::num(x as f64, decimals)
}

/// Signed number: +2,1 or −3.
pub(super) fn signed(x: f32, decimals: usize) -> String {
    let s = fr(x.abs(), decimals);
    if x < -1e-6 { format!("−{s}") } else { format!("+{s}") }
}

/// Duration: 120 ms, 2,35 s, 1:04,2.
pub(super) fn fmt_secs(s: f32) -> String {
    if s < 1.0 {
        format!("{} ms", fr(s * 1_000.0, 0))
    } else if s < 60.0 {
        format!("{} s", fr(s, 2))
    } else {
        format!("{}:{:0>4}", (s / 60.0) as u32, fr(s % 60.0, 1))
    }
}

/// Clock position: m:ss.mmm.
pub(super) fn fmt_clock(s: f64) -> String {
    let ms = (s * 1_000.0).round().max(0.0) as u64;
    let sep = if ponzik_i18n::num(0.5, 1).contains(',') { ',' } else { '.' };
    format!("{}:{:02}{sep}{:03}", ms / 60_000, ms / 1_000 % 60, ms % 1_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        let project = ponzik_project::demo_project();
        let (_engine, handle) = project.build_engine(48_000.0, &|_| None);
        App::new(handle, project, 48_000)
    }

    fn ramp(n: usize) -> Arc<Sample> {
        let left: Vec<f32> = (0..n).map(|i| i as f32 / n as f32).collect();
        Arc::new(Sample { name: "ramp".into(), sample_rate: 48_000.0, right: left.clone(), left, markers: Vec::new() })
    }

    fn doc_of(app: &App) -> &Doc {
        app.audio_edit.doc.as_ref().unwrap()
    }

    #[test]
    fn cut_paste_crop_and_silence_keep_markers_in_place() {
        let mut app = app();
        app.open_sample_in_editor(ramp(1_000), None, Source::Browser);
        app.audio_edit.snap_zero = false;
        if let Some(d) = app.audio_edit.doc.as_mut() {
            d.slices = vec![100, 500, 900];
            d.markers.push(Marker { pos: 800, name: "m".into() });
        }
        app.ae_select(400, 600);
        app.ae_cut();
        let d = doc_of(&app);
        assert_eq!(d.len(), 800);
        assert_eq!(d.slices, vec![100, 700]);
        assert_eq!(d.markers[0].pos, 600);
        assert_eq!(app.audio_edit.clipboard.as_ref().unwrap().0.len(), 200);
        // Paste at the cursor, where the cut was.
        app.ae_paste();
        let d = doc_of(&app);
        assert_eq!(d.len(), 1_000);
        assert_eq!(d.sample.left[400], 0.4);
        assert_eq!(d.sel, Some((400, 600)));
        // Undo the paste, then the cut: the original comes back.
        assert!(app.ae_undo(false) && app.ae_undo(false));
        assert_eq!(doc_of(&app).sample.left, ramp(1_000).left);
        assert!(app.ae_undo(true));
        assert_eq!(doc_of(&app).len(), 800);
        app.ae_undo(false);
        app.ae_select(200, 300);
        app.ae_crop();
        assert_eq!(doc_of(&app).len(), 100);
        assert_eq!(doc_of(&app).sample.left[0], 0.2);
        app.ae_undo(false);
        app.ae_select(0, 0);
        if let Some(d) = app.audio_edit.doc.as_mut() {
            d.cursor = 10;
        }
        app.audio_edit.silence_ms = 1.0;
        app.ae_insert_silence();
        let d = doc_of(&app);
        assert_eq!(d.len(), 1_048);
        assert!(d.sample.left[10..58].iter().all(|&v| v == 0.0));
    }

    #[test]
    fn processing_works_on_the_selection_only_and_undoes() {
        let mut app = app();
        let tone: Vec<f32> = (0..48_000).map(|i| 0.25 * (i as f32 * 0.05).sin()).collect();
        let s = Arc::new(Sample { name: "t".into(), sample_rate: 48_000.0, right: tone.clone(), left: tone, markers: Vec::new() });
        app.open_sample_in_editor(s.clone(), None, Source::Browser);
        app.ae_select(10_000, 20_000);
        app.audio_edit.norm = NormMode::Peak;
        app.audio_edit.peak_target = -1.0;
        app.ae_normalize();
        let d = doc_of(&app);
        let peak = edit::peak(&d.sample.left[10_000..20_000]);
        assert!((20.0 * peak.log10() + 1.0).abs() < 0.01);
        assert_eq!(d.sample.left[..10_000], s.left[..10_000], "outside untouched");
        app.ae_fade(false);
        assert!(doc_of(&app).sample.left[19_999].abs() < 0.01);
        app.ae_reverse();
        app.ae_undo(false);
        app.ae_undo(false);
        app.ae_undo(false);
        assert_eq!(doc_of(&app).sample.left, s.left);
    }

    #[test]
    fn noise_is_learnt_then_reduced() {
        let mut app = app();
        let s = Arc::new(demo_voice(48_000.0));
        app.open_sample_in_editor(s.clone(), None, Source::Browser);
        app.ae_select(0, 24_000);
        app.ae_learn_noise();
        assert!(app.audio_edit.noise.is_some());
        app.ae_select(0, 0);
        app.ae_denoise();
        let d = doc_of(&app);
        assert_eq!(d.len(), s.len());
        let before = edit::rms(&s.left[2_000..24_000]);
        let after = edit::rms(&d.sample.left[2_000..24_000]);
        assert!(20.0 * (before / after).log10() > 12.0, "{before} → {after}");
    }

    #[test]
    fn snap_lands_on_zero_crossings_and_beats() {
        let mut app = app();
        let tone: Vec<f32> = (0..48_000).map(|i| (std::f32::consts::TAU * 100.0 * i as f32 / 48_000.0).sin()).collect();
        let s = Arc::new(Sample { name: "t".into(), sample_rate: 48_000.0, right: tone.clone(), left: tone, markers: Vec::new() });
        app.open_sample_in_editor(s, None, Source::Browser);
        let m = Modifiers::default();
        app.audio_edit.snap_zero = true;
        assert_eq!(app.ae_snap(250, m), 240);
        app.audio_edit.snap_zero = false;
        app.audio_edit.snap_beat = true;
        app.audio_edit.doc.as_mut().unwrap().bpm = Some(120.0);
        // Sixteenths at 120 BPM: 6 000 frames.
        assert_eq!(app.ae_snap(8_900, m), 6_000);
        assert_eq!(app.ae_snap(9_100, m), 12_000);
        assert_eq!(app.ae_snap(9_100, Modifiers { alt: true, ..m }), 9_100);
    }

    #[test]
    fn slices_go_to_the_slicer_pads_midi_and_a_drum_pattern() {
        let mut app = app();
        let dir = std::env::temp_dir().join(format!("ponzik-ae-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        app.project_path = Some(dir.join("song.ponzik"));
        app.open_sample_in_editor(Arc::new(super::super::samplelab::demo_break(48_000.0)), None, Source::Browser);
        app.ae_detect_slices();
        let n = doc_of(&app).bounds().len() - 1;
        assert!(n >= 16, "{n} slices");
        let bpm = doc_of(&app).bpm.unwrap();
        assert!((bpm - 92.0).abs() < 1.0 || (bpm - 184.0).abs() < 2.0 || (bpm - 46.0).abs() < 1.0, "{bpm}");
        let channels = app.project.channels.len();
        app.ae_to_slicer(true);
        assert_eq!(app.project.channels.len(), channels + 1);
        let ch = channels;
        let pat = &app.project.patterns[app.project.current_pattern];
        assert_eq!(pat.notes[ch].len(), n);
        assert_eq!(pat.notes[ch][0].key, 60);
        app.ae_to_pads();
        let pads = &app.project.channels[ch + 1].instrument;
        assert_eq!(pads.kind, "drum_pads");
        assert_eq!(pads.pads.iter().filter(|p| !p.is_empty()).count(), n.min(16));
        app.ae_drum_pattern();
        let kit = &app.project.channels[ch + 2].instrument;
        assert!(!kit.pads[0].is_empty() && !kit.pads[2].is_empty(), "a kick and a snare pad");
        let notes = &app.project.patterns[app.project.current_pattern].notes[ch + 2];
        assert!(notes.iter().any(|n| n.key == 36) && notes.iter().any(|n| n.key == 38));
        assert_eq!(notes[0].key, 36, "the break starts on a kick");
        let files = app.ae_export_slices();
        assert_eq!(files.len(), n);
        assert!(files.iter().all(|f| Path::new(f).starts_with(dir.join("Audio"))));
        let flac = app.ae_save(true).unwrap();
        let back = ponzik_engine::wav::load_wav(&flac).unwrap();
        assert_eq!(back.len(), doc_of(&app).len());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn drum_hits_are_told_apart() {
        let sr = 48_000.0;
        let n = 4_800;
        let tau = std::f32::consts::TAU;
        let kick: Vec<f32> = (0..n).map(|i| (tau * 55.0 * i as f32 / sr).sin() * (-(i as f32) / 2_000.0).exp()).collect();
        let mut seed = 9u32;
        let mut noise = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1u32 << 23) as f32 - 1.0
        };
        let mut prev = 0.0f32;
        let hat: Vec<f32> = (0..n)
            .map(|i| {
                let w = noise();
                let y = w - prev;
                prev = w;
                y * (-(i as f32) / 300.0).exp()
            })
            .collect();
        let snare: Vec<f32> = (0..n).map(|i| (0.5 * (tau * 190.0 * i as f32 / sr).sin() + 0.7 * noise()) * (-(i as f32) / 900.0).exp()).collect();
        assert_eq!(classify(&kick, sr), Hit::Kick);
        assert_eq!(classify(&hat, sr), Hit::Hat);
        assert_eq!(classify(&snare, sr), Hit::Snare);
    }

    #[test]
    fn formats() {
        assert_eq!(fmt_clock(65.0421), "1:05.042".replace('.', if ponzik_i18n::num(0.5, 1).contains(',') { "," } else { "." }));
        assert!(signed(-3.0, 0).starts_with('−') && signed(2.0, 0).starts_with('+'));
    }
}
