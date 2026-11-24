//! Note-based pitch and timing edits of recorded audio (the Pitch Editor).
//!
//! The edits of a recording are saved in the project, per file: every clip,
//! take and comp segment that plays that file plays it corrected. They are
//! non-destructive: the file is never written. A render of the corrected
//! file (same length, same rate) is made off the interface thread, kept in a
//! process-wide cache and in a cache folder on disk (by a hash of the file's
//! content and of the edits), and the playlist plays it in place of the file
//! once it is there; until then the file plays as recorded.
//!
//! The analysis (pitch track and notes) is cached the same way, by the hash
//! of the file's content alone, so opening the editor on a take again or
//! after a restart is instant.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, Weak};

use ponzik_engine::Sample;
use ponzik_engine::pitchedit::{self, Analysis, NoteEdit};

use crate::Project;
use crate::json::Json;

/// The pitch edits of one file.
#[derive(Clone, Debug, PartialEq)]
pub struct PitchEditDef {
    pub path: String,
    /// Every note of the file, edited or not, sorted by start.
    pub notes: Vec<NoteEdit>,
}

impl PitchEditDef {
    pub fn is_identity(&self) -> bool {
        pitchedit::is_identity(&self.notes)
    }
}

impl Project {
    pub fn pitch_edit(&self, path: &str) -> Option<&PitchEditDef> {
        self.pitch_edits.iter().find(|d| d.path == path)
    }

    /// Sets the notes of a file; edits that change nothing are forgotten.
    pub fn set_pitch_edit(&mut self, path: &str, notes: Vec<NoteEdit>) {
        let identity = pitchedit::is_identity(&notes);
        match self.pitch_edits.iter().position(|d| d.path == path) {
            Some(i) if identity => {
                self.pitch_edits.remove(i);
            }
            Some(i) => self.pitch_edits[i].notes = notes,
            None if identity => {}
            None => self.pitch_edits.push(PitchEditDef { path: path.to_string(), notes }),
        }
    }

    /// What plays for the file at `path`: its corrected render when there
    /// are edits and the render is done (the previous render of the file
    /// while the new one is being made), else the file.
    pub fn tuned_sample(&self, path: &str, sample: Arc<Sample>) -> Arc<Sample> {
        if bypassed() {
            return sample;
        }
        let Some(def) = self.pitch_edit(path).filter(|d| !d.is_identity()) else { return sample };
        let content = content_key(&sample);
        cached(job_key(content, &def.notes)).or_else(|| with_cache(|c| c.last.get(&content).map(|l| l.1.clone()))).unwrap_or(sample)
    }

    /// True while the render the edits of `path` need is not there yet.
    pub fn tune_pending(&self, path: &str, sample: &Arc<Sample>) -> bool {
        self.pitch_edit(path).filter(|d| !d.is_identity()).is_some_and(|d| cached(job_key(content_key(sample), &d.notes)).is_none())
    }

    /// The renders the project's pitch edits need.
    pub fn tune_jobs(&self, loader: crate::SampleLoader) -> Vec<TuneJob> {
        self.pitch_edits
            .iter()
            .filter(|d| !d.is_identity())
            .filter_map(|d| Some(TuneJob::new(&d.path, loader(&d.path)?, d.notes.clone())))
            .collect()
    }

    /// Renders now what the pitch edits need (exports and offline renders).
    pub fn render_tunes(&self, loader: crate::SampleLoader) {
        for job in self.tune_jobs(loader) {
            if cached(job.key).is_none() {
                job.run(None, None);
            }
        }
    }
}

/// One render: a file and its edits.
#[derive(Clone, Debug)]
pub struct TuneJob {
    pub key: u64,
    pub path: String,
    pub sample: Arc<Sample>,
    pub notes: Vec<NoteEdit>,
}

impl TuneJob {
    pub fn new(path: &str, sample: Arc<Sample>, notes: Vec<NoteEdit>) -> TuneJob {
        let key = job_key(content_key(&sample), &notes);
        TuneJob { key, path: path.to_string(), sample, notes }
    }

    /// Renders (from the disk cache, or from the previous render of the file
    /// when there is one, redoing only what changed) and caches the result.
    /// `None` when cancelled.
    pub fn run(&self, progress: Option<&AtomicU32>, cancel: Option<&AtomicBool>) -> Option<Arc<Sample>> {
        if let Some(s) = cached(self.key) {
            return Some(s);
        }
        if let Some(s) = load_render(self.key, &self.sample) {
            return Some(store_render(self.key, content_key(&self.sample), &self.notes, s, false));
        }
        let analysis = analyze(&self.sample, None, cancel)?;
        let ck = content_key(&self.sample);
        let base = with_cache(|c| c.last.get(&ck).cloned());
        let base_ref = base.as_ref().map(|(n, s)| (n.as_slice(), &**s));
        let (out, _) = pitchedit::render_regions(&self.sample, &analysis, &self.notes, base_ref, progress, cancel)?;
        Some(store_render(self.key, ck, &self.notes, out, true))
    }
}

// --- File format ---

/// `[{"path": …, "notes": [{start, end, new_start, new_end, centre, pitch, drift, vibrato, formant, gain}, …]}, …]`
pub(crate) fn edits_json(defs: &[PitchEditDef]) -> Json {
    let num = Json::Num;
    Json::Arr(
        defs.iter()
            .map(|d| {
                let notes = d
                    .notes
                    .iter()
                    .map(|n| {
                        let mut o = vec![("start", num(n.start)), ("end", num(n.end))];
                        if n.time_edited() {
                            o.extend([("new_start", num(n.new_start)), ("new_end", num(n.new_end))]);
                        }
                        o.extend([("centre", num(f64::from(n.centre))), ("pitch", num(f64::from(n.pitch)))]);
                        for (k, v, default) in [("drift", n.drift, 0.0), ("vibrato", n.vibrato, 1.0), ("formant", n.formant, 0.0), ("gain", n.gain, 0.0)] {
                            if v != default {
                                o.push((k, num(f64::from(v))));
                            }
                        }
                        Json::Obj(o.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
                    })
                    .collect();
                Json::Obj(vec![("path".into(), Json::Str(d.path.clone())), ("notes".into(), Json::Arr(notes))])
            })
            .collect(),
    )
}

/// Reads [`edits_json`]: broken notes are skipped, missing values take their defaults.
pub(crate) fn load_edits(v: Option<&Json>) -> Vec<PitchEditDef> {
    fn arr(v: Option<&Json>) -> &[Json] {
        v.and_then(Json::as_arr).unwrap_or(&[])
    }
    let num = |o: &Json, k: &str| o.get(k).and_then(Json::as_f64).filter(|x| x.is_finite());
    let mut out: Vec<PitchEditDef> = Vec::new();
    for d in arr(v) {
        let Some(path) = d.get("path").and_then(Json::as_str).filter(|p| !p.is_empty()) else { continue };
        let mut notes: Vec<NoteEdit> = arr(d.get("notes"))
            .iter()
            .filter_map(|n| {
                let (start, end) = (num(n, "start")?.max(0.0), num(n, "end")?);
                let centre = num(n, "centre")?.clamp(0.0, 127.0) as f32;
                if end <= start {
                    return None;
                }
                let f = |k: &str, default: f32, lo: f32, hi: f32| num(n, k).map_or(default, |v| (v as f32).clamp(lo, hi));
                let new_start = num(n, "new_start").unwrap_or(start).max(0.0);
                let new_end = num(n, "new_end").unwrap_or(end).max(new_start + 0.005);
                Some(NoteEdit {
                    start,
                    end,
                    new_start,
                    new_end,
                    centre,
                    pitch: f("pitch", centre, 0.0, 127.0),
                    drift: f("drift", 0.0, 0.0, 1.0),
                    vibrato: f("vibrato", 1.0, 0.0, 3.0),
                    formant: f("formant", 0.0, -12.0, 12.0),
                    gain: f("gain", 0.0, -48.0, 24.0),
                })
            })
            .collect();
        notes.sort_by(|a, b| a.start.total_cmp(&b.start));
        if !notes.is_empty() && !out.iter().any(|o| o.path == path) {
            out.push(PitchEditDef { path: path.to_string(), notes });
        }
    }
    out
}

// --- Keys ---

struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Fnv(0xcbf2_9ce4_8422_2325)
    }

    #[inline]
    fn word(&mut self, v: u64) {
        self.0 = (self.0 ^ v).wrapping_mul(0x0000_0100_0000_01b3);
        self.0 ^= self.0 >> 29;
    }
}

/// Identity of a sample's content (every sample, its rate), remembered per
/// loaded sample: hashing a long take once is enough.
pub fn content_key(sample: &Arc<Sample>) -> u64 {
    let ptr = Arc::as_ptr(sample) as usize;
    {
        let keys = KEYS.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, _, k)) = keys.iter().find(|(p, w, _)| *p == ptr && w.strong_count() > 0) {
            return *k;
        }
    }
    let mut h = Fnv::new();
    h.word(u64::from(sample.sample_rate.to_bits()));
    h.word(sample.len() as u64);
    let right: &[f32] = if sample.right.len() == sample.len() { &sample.right } else { &sample.left };
    for (l, r) in sample.left.iter().zip(right) {
        h.word(u64::from(l.to_bits()) << 32 | u64::from(r.to_bits()));
    }
    let key = h.0;
    let mut keys = KEYS.lock().unwrap_or_else(|e| e.into_inner());
    keys.retain(|(_, w, _)| w.strong_count() > 0);
    keys.push((ptr, Arc::downgrade(sample), key));
    key
}

/// Identity of a render: the file's content and its edits.
pub fn job_key(content: u64, notes: &[NoteEdit]) -> u64 {
    let mut h = Fnv::new();
    h.word(content);
    h.word(u64::from(pitchedit::ANALYSIS_VERSION));
    for n in notes {
        for v in [n.start, n.end, n.new_start, n.new_end] {
            h.word(v.to_bits());
        }
        for v in [n.centre, n.pitch, n.drift, n.vibrato, n.formant, n.gain] {
            h.word(u64::from(v.to_bits()));
        }
    }
    h.0
}

// --- Caches ---

static KEYS: Mutex<Vec<(usize, Weak<Sample>, u64)>> = Mutex::new(Vec::new());
static BYPASS: AtomicBool = AtomicBool::new(false);

#[derive(Default)]
struct Cache {
    analyses: HashMap<u64, Arc<Analysis>>,
    renders: HashMap<u64, (Arc<Sample>, u64)>,
    /// Last render of each file (by content): the base of the next one.
    last: HashMap<u64, (Vec<NoteEdit>, Arc<Sample>)>,
    clock: u64,
    dir: Option<PathBuf>,
}

static CACHE: Mutex<Option<Cache>> = Mutex::new(None);

fn with_cache<T>(f: impl FnOnce(&mut Cache) -> T) -> T {
    let mut guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    f(guard.get_or_insert_with(Cache::default))
}

/// Folder where analyses and renders are kept between sessions (`None`: memory only).
pub fn set_cache_dir(dir: Option<PathBuf>) {
    with_cache(|c| c.dir = dir);
}

fn cache_file(name: &str) -> Option<PathBuf> {
    with_cache(|c| c.dir.clone()).map(|d| d.join(name))
}

/// A/B: the files play as recorded while set (the edits stay).
pub fn set_bypass(on: bool) {
    BYPASS.store(on, Ordering::Relaxed);
}

pub fn bypassed() -> bool {
    BYPASS.load(Ordering::Relaxed)
}

/// The analysis of a file, if it was made (in memory or on disk).
pub fn analysis(sample: &Arc<Sample>) -> Option<Arc<Analysis>> {
    let key = content_key(sample);
    if let Some(a) = with_cache(|c| c.analyses.get(&key).cloned()) {
        return Some(a);
    }
    let file = cache_file(&format!("{key:016x}.pzpa"))?;
    let a = Arc::new(Analysis::from_bytes(&std::fs::read(file).ok()?)?);
    with_cache(|c| c.analyses.insert(key, a.clone()));
    Some(a)
}

/// The analysis of a file, made now if needed (`None` when cancelled).
pub fn analyze(sample: &Arc<Sample>, progress: Option<&AtomicU32>, cancel: Option<&AtomicBool>) -> Option<Arc<Analysis>> {
    if let Some(a) = analysis(sample) {
        return Some(a);
    }
    let a = Arc::new(pitchedit::analyze_with(&sample.left, &sample.right, sample.sample_rate, progress, cancel)?);
    let key = content_key(sample);
    if let Some(file) = cache_file(&format!("{key:016x}.pzpa")) {
        let _ = file.parent().map(std::fs::create_dir_all);
        let _ = std::fs::write(file, a.to_bytes());
    }
    with_cache(|c| c.analyses.insert(key, a.clone()));
    Some(a)
}

/// The render of a job, if it is done.
pub fn cached(key: u64) -> Option<Arc<Sample>> {
    with_cache(|c| {
        c.clock += 1;
        let clock = c.clock;
        c.renders.get_mut(&key).map(|e| {
            e.1 = clock;
            e.0.clone()
        })
    })
}

fn render_file(key: u64) -> Option<PathBuf> {
    cache_file(&format!("{key:016x}.wav"))
}

fn load_render(key: u64, source: &Sample) -> Option<Sample> {
    let s = ponzik_engine::wav::load_wav(render_file(key)?).ok()?;
    (s.len() == source.len() && (s.sample_rate - source.sample_rate).abs() < 0.5).then_some(s)
}

fn store_render(key: u64, content: u64, notes: &[NoteEdit], s: Sample, write: bool) -> Arc<Sample> {
    if write && let Some(file) = render_file(key) {
        let _ = file.parent().map(std::fs::create_dir_all);
        let _ = ponzik_engine::wav::write_sample(file, &s);
    }
    let s = Arc::new(s);
    with_cache(|c| {
        c.clock += 1;
        let used = c.clock;
        c.renders.insert(key, (s.clone(), used));
        c.last.insert(content, (notes.to_vec(), s.clone()));
    });
    s
}

/// Forgets the renders nobody needs: keeps `wanted` and the `spare` most
/// recently used others (an undo finds them again).
pub fn prune(wanted: &[u64], spare: usize) {
    with_cache(|c| {
        let mut others: Vec<(u64, u64)> = c.renders.iter().filter(|(k, _)| !wanted.contains(k)).map(|(k, e)| (e.1, *k)).collect();
        others.sort_unstable_by(|a, b| b.cmp(a));
        for (_, k) in others.into_iter().skip(spare) {
            c.renders.remove(&k);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AudioClipDef, RenderMode, render_song};
    use ponzik_engine::effects::autotune::voice::{Style, Sung, sing};

    const SR: f32 = 48_000.0;

    fn take() -> Arc<Sample> {
        let notes = [Sung { start: 0.1, dur: 0.6, midi: 57.0, vowel: 0 }, Sung { start: 0.8, dur: 0.6, midi: 60.0, vowel: 1 }];
        let x = sing(SR, &notes, &Style::default(), 1.6);
        Arc::new(Sample { name: "take".into(), sample_rate: SR, right: x.clone(), left: x, markers: Vec::new() })
    }

    #[test]
    fn edits_round_trip_and_identity_is_forgotten() {
        let s = take();
        let a = pitchedit::analyze(&s.left, &s.right, SR);
        assert_eq!(a.notes.len(), 2);
        let mut p = Project::empty();
        let mut notes: Vec<NoteEdit> = a.notes.iter().map(NoteEdit::from_note).collect();
        p.set_pitch_edit("take.wav", notes.clone());
        assert!(p.pitch_edits.is_empty());
        notes[1].pitch += 2.0;
        notes[0].drift = 0.5;
        notes[0].new_end += 0.05;
        p.set_pitch_edit("take.wav", notes.clone());
        assert_eq!(p.pitch_edits.len(), 1);
        let back = crate::io::load(&crate::io::save(&p)).unwrap();
        assert_eq!(back.pitch_edits, p.pitch_edits);
        // Broken entries are skipped, missing fields take their defaults.
        let json = r#"{"format": "ponzik-project", "name": "x", "pitch_edits": [{"path": "a.wav", "notes": [{"start": 1, "end": 2, "centre": 60, "pitch": 62}, {"start": "x"}]}, {"notes": []}]}"#;
        let p = crate::io::load(json).unwrap();
        assert_eq!(p.pitch_edits.len(), 1);
        let n = p.pitch_edits[0].notes[0];
        assert_eq!((n.new_start, n.new_end, n.vibrato, n.gain), (1.0, 2.0, 1.0, 0.0));
        assert_eq!(p.pitch_edits[0].notes.len(), 1);
    }

    #[test]
    fn a_clip_plays_its_corrected_take() {
        let s = take();
        let a = pitchedit::analyze(&s.left, &s.right, SR);
        let mut p = Project::empty();
        p.audio_clips.push(AudioClipDef { path: "take.wav".into(), track: 1, len: 4 * ponzik_engine::PPQ, ..Default::default() });
        let mut notes: Vec<NoteEdit> = a.notes.iter().map(NoteEdit::from_note).collect();
        notes[1].pitch += 3.0;
        p.set_pitch_edit("take.wav", notes.clone());
        let loader = |path: &str| (path == "take.wav").then(|| s.clone());
        let mut plain = p.clone();
        plain.pitch_edits.clear();
        let dry = render_song(&plain, SR as u32, &loader, RenderMode::Song);
        // An export renders what is missing, then plays it.
        let out = render_song(&p, SR as u32, &loader, RenderMode::Song);
        let job = TuneJob::new("take.wav", s.clone(), notes);
        let r = cached(job.key).expect("rendered for the export");
        assert_ne!(r.left, s.left);
        assert!(Arc::ptr_eq(&p.tuned_sample("take.wav", s.clone()), &r));
        // A/B: the original plays while bypassed.
        set_bypass(true);
        assert!(Arc::ptr_eq(&p.tuned_sample("take.wav", s.clone()), &s));
        set_bypass(false);
        // The export holds the moved note, the first one as recorded.
        let diff = |t: f32| (0..4_800).map(|i| (out[2 * ((t * SR) as usize + i)] - dry[2 * ((t * SR) as usize + i)]).abs()).sum::<f32>();
        assert!(diff(1.1) > 1.0, "{}", diff(1.1));
        assert!(diff(0.4) < 1e-3, "{}", diff(0.4));
    }
}
