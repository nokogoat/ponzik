//! L'Atelier : l'éditeur d'échantillons de Ponzik, pensé pour sampler.
//!
//! On y ouvre un WAV du navigateur, un clip audio de la playlist ou
//! l'échantillon d'un Sampler ou d'un Découpeur. Grande forme d'onde
//! zoomable, colorée par la brillance du son ; découpe aux transitoires, à la
//! grille ou à la main (gestes de FL : clic pour poser, glisser pour déplacer,
//! clic droit maintenu pour effacer) ; chaque tranche a sa couleur et son pad.
//! Les tranches partent en un clic dans un Découpeur (notes 60 + k), avec, si
//! on veut, un pattern qui les rejoue dans l'ordre d'origine (le « flip »).
//!
//! Rognage, fondus et gain ne détruisent rien : ils sont rendus à
//! l'enregistrement. Inverser, transposer et caler au tempo réécrivent le
//! tampon de travail ; Ctrl+Z les annule dans l'Atelier, et ce qui part dans
//! le projet passe par son historique.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ponzik_dsp::onset::{self, OnsetAnalysis};
use ponzik_dsp::stretch;
use ponzik_dsp::tempo;
use ponzik_dsp::util::db_to_gain;
use ponzik_engine::audition::Audition;
use ponzik_engine::{Command, Note, PPQ, Sample, TICKS_PER_BAR};
use ponzik_project::InstrumentDef;
use ponzik_ui::{Canvas, Color, Path as Shape, Rect};
use ponzik_window::{Key, Modifiers};

use super::Input;
use super::app::{App, Focus, View};
use super::dnd::{DropTarget, Payload};
use super::playlist::pattern_color;
use super::theme::*;
use super::widgets::{Icon, draw_icon, wid};

/// Images par paquet de crêtes.
const BUCKET: usize = 64;
/// Le Découpeur joue 64 tranches au plus.
const MAX_SLICES: usize = 64;
/// Écart minimal entre deux marqueurs, en images.
const MIN_GAP: usize = 64;
/// Divisions de la grille, en notes par mesure.
const DIVISIONS: [(u32, &str); 4] = [(4, "1/4"), (8, "1/8"), (16, "1/16"), (32, "1/32")];
/// Mode « Marqueurs » du Découpeur.
const SLICER_MODE: u32 = 0;
const SLICER_MARKERS: f32 = 2.0;
const INSPECTOR_W: f32 = 236.0;
const HANDLE: f32 = 7.0;

/// D'où vient l'échantillon ouvert, pour le lui rendre.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Source {
    #[default]
    Browser,
    Channel(usize),
    Clip(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Chop {
    Transients,
    Grid,
    Manual,
}

/// État éditable (et annulable) de l'Atelier.
#[derive(Clone)]
struct Doc {
    /// Tampon de travail : réécrit par les opérations destructives.
    sample: Arc<Sample>,
    /// Coupes strictement à l'intérieur du rognage, triées.
    markers: Vec<usize>,
    trim: (usize, usize),
    fade_in: usize,
    fade_out: usize,
    /// Courbes des fondus, de −1 (rapide) à 1 (lent).
    curve_in: f32,
    curve_out: f32,
    gain_db: f32,
    chop: Chop,
    sensitivity: f32,
    division: usize,
    /// Tempo d'origine, détecté ou saisi.
    bpm: Option<f32>,
}

impl Doc {
    fn new(sample: Arc<Sample>) -> Self {
        let len = sample.len();
        let markers: Vec<usize> = sample.markers.iter().copied().filter(|&m| m > 0 && m < len).collect();
        let chop = if markers.is_empty() { Chop::Transients } else { Chop::Manual };
        Self {
            sample,
            markers,
            trim: (0, len),
            fade_in: 0,
            fade_out: 0,
            curve_in: 0.0,
            curve_out: 0.0,
            gain_db: 0.0,
            chop,
            sensitivity: 0.5,
            division: 2,
            bpm: None,
        }
    }

    /// Bornes des tranches : début du rognage, coupes, fin.
    fn bounds(&self) -> Vec<usize> {
        let (a, b) = self.trim;
        let mut v = vec![a];
        v.extend(self.markers.iter().copied().filter(|&m| m > a + MIN_GAP / 2 && m + MIN_GAP / 2 < b));
        v.push(b);
        v
    }

    fn slices(&self) -> usize {
        self.bounds().len() - 1
    }

    /// Gain (gain, fondus) à l'image `i` du rognage.
    fn envelope(&self, frame: usize) -> f32 {
        let (a, b) = self.trim;
        if frame < a || frame >= b {
            return 0.0;
        }
        let g = db_to_gain(self.gain_db);
        let i = frame - a;
        let n = b - a;
        let mut e = 1.0f32;
        if self.fade_in > 0 && i < self.fade_in {
            e = e.min(shape(i as f32 / self.fade_in as f32, self.curve_in));
        }
        if self.fade_out > 0 && n - i <= self.fade_out {
            e = e.min(shape((n - i) as f32 / self.fade_out as f32, self.curve_out));
        }
        g * e
    }

    fn render_key(&self) -> RenderKey {
        (
            Arc::as_ptr(&self.sample) as usize,
            self.trim,
            self.fade_in,
            self.fade_out,
            self.curve_in.to_bits(),
            self.curve_out.to_bits(),
            self.gain_db.to_bits(),
        )
    }

    /// Rendu : rognage, gain et fondus appliqués ; marqueurs relatifs au début.
    fn render(&self) -> Sample {
        let (a, b) = self.trim;
        let s = &self.sample;
        let (mut left, mut right) = (Vec::with_capacity(b - a), Vec::with_capacity(b - a));
        for i in a..b {
            let e = self.envelope(i);
            left.push(s.left[i] * e);
            right.push(s.right[i] * e);
        }
        let mut markers = vec![0];
        markers.extend(self.bounds()[1..].iter().map(|&m| m - a).filter(|&m| m < b - a));
        Sample { name: s.name.clone(), sample_rate: s.sample_rate, left, right, markers }
    }
}

/// Ce dont dépend le rendu : tampon, rognage, fondus, courbes, gain.
type RenderKey = (usize, (usize, usize), usize, usize, u32, u32, u32);

/// Fondu : `t` dans [0, 1], courbe de −1 (rapide) à 1 (lente).
fn shape(t: f32, curve: f32) -> f32 {
    t.clamp(0.0, 1.0).powf(4f32.powf(curve))
}

/// Analyses du tampon de travail, refaites quand il change.
struct Cache {
    key: usize,
    mono: Vec<f32>,
    onsets: OnsetAnalysis,
    /// Crêtes (min, max) des deux canaux par paquet de [`BUCKET`] images.
    peaks: Vec<(f32, f32)>,
    /// Brillance 0 à 1 par paquet (rapport énergie de la dérivée / énergie).
    bright: Vec<f32>,
}

impl Cache {
    fn new(sample: &Arc<Sample>) -> Self {
        let mono = onset::mono(&sample.left, &sample.right);
        let onsets = OnsetAnalysis::new(&mono, sample.sample_rate);
        let peaks = sample
            .left
            .chunks(BUCKET)
            .zip(sample.right.chunks(BUCKET))
            .map(|(l, r)| l.iter().chain(r).fold((0.0f32, 0.0f32), |(lo, hi), &v| (lo.min(v), hi.max(v))))
            .collect::<Vec<_>>();
        let n = peaks.len();
        let mut raw = vec![0.0f32; n];
        for (j, b) in raw.iter_mut().enumerate() {
            let lo = (j * BUCKET).saturating_sub(96);
            let hi = (j * BUCKET + BUCKET + 96).min(mono.len());
            let (mut e, mut d) = (0.0f32, 0.0f32);
            for i in lo.max(1)..hi {
                e += mono[i] * mono[i];
                let diff = mono[i] - mono[i - 1];
                d += diff * diff;
            }
            *b = if e < 1e-7 { 0.0 } else { ((d / e).sqrt() / 0.01).max(1.0).ln() / 120f32.ln() };
        }
        let bright = (0..n)
            .map(|j| {
                let (lo, hi) = (j.saturating_sub(2), (j + 3).min(n));
                (raw[lo..hi].iter().sum::<f32>() / (hi - lo) as f32).clamp(0.0, 1.0)
            })
            .collect();
        Self { key: Arc::as_ptr(sample) as usize, mono, onsets, peaks, bright }
    }
}

/// Geste en cours dans l'Atelier.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Grab {
    Marker(usize),
    FadeIn,
    FadeOut,
    CurveIn,
    CurveOut,
    TrimStart,
    TrimEnd,
    /// Zone de boucle tirée depuis cette image.
    Loop(usize),
    /// Balayage de la règle : dernière position jouée (x).
    Scrub(f32),
    Overview,
    /// Pad saisi : tranche, point de départ.
    Pad(usize, f32, f32),
}

pub struct Lab {
    pub source: Source,
    pub path: Option<String>,
    name: String,
    doc: Option<Doc>,
    undo: Vec<(String, Doc)>,
    redo: Vec<(String, Doc)>,
    last_edit: Option<(String, f32)>,
    cache: Option<Cache>,
    /// Vue visée (début, longueur en images) et vue affichée, qui la rejoint en douceur.
    view: (f64, f64),
    shown: (f64, f64),
    last_time: f32,
    pub selected: usize,
    loop_region: Option<(usize, usize)>,
    spectral: bool,
    preview: Option<(RenderKey, Arc<Sample>)>,
    grab: Option<Grab>,
    grab_value: f32,
    /// Début du rognage au lancement de la pré-écoute : la position lue s'y ajoute.
    play_offset: Option<usize>,
    pad_scroll: f32,
}

impl Default for Lab {
    fn default() -> Self {
        Self {
            source: Source::Browser,
            path: None,
            name: String::new(),
            doc: None,
            undo: Vec::new(),
            redo: Vec::new(),
            last_edit: None,
            cache: None,
            view: (0.0, 1.0),
            shown: (0.0, 1.0),
            last_time: 0.0,
            selected: 0,
            loop_region: None,
            spectral: true,
            preview: None,
            grab: None,
            grab_value: 0.0,
            play_offset: None,
            pad_scroll: 0.0,
        }
    }
}

/// Clé d'animation d'une poignée.
fn grab_key(g: Grab) -> usize {
    match g {
        Grab::FadeIn => 1,
        Grab::FadeOut => 2,
        Grab::CurveIn => 3,
        Grab::CurveOut => 4,
        Grab::TrimStart => 5,
        Grab::TrimEnd => 6,
        _ => 0,
    }
}

/// Dossier des échantillons édités.
fn samples_dir() -> PathBuf {
    super::compat::projects_dir().join("Échantillons")
}

/// Nom de fichier propre.
fn clean_name(name: &str) -> String {
    let s: String = name.chars().map(|c| if c.is_alphanumeric() || " -_".contains(c) { c } else { '_' }).collect();
    let s = s.trim().to_string();
    if s.is_empty() { "Sample".into() } else { s }
}

/// `dir/stem.wav`, ou `dir/stem 2.wav`… s'il existe déjà.
fn unique_path(dir: &Path, stem: &str) -> PathBuf {
    let first = dir.join(format!("{stem}.wav"));
    if !first.exists() {
        return first;
    }
    (2..10_000).map(|n| dir.join(format!("{stem} {n}.wav"))).find(|p| !p.exists()).unwrap_or(first)
}

/// Nom de base sans le suffixe « atelier » déjà posé.
fn base_name(name: &str) -> String {
    let mut s = name.trim();
    if let Some((head, tail)) = s.rsplit_once(' ')
        && tail.chars().all(|c| c.is_ascii_digit())
        && head.ends_with(" lab")
    {
        s = head;
    }
    s.strip_suffix(" lab").unwrap_or(s).to_string()
}

/// Nombre à la française.
fn fr(x: f32, decimals: usize) -> String {
    format!("{x:.decimals$}")
}

fn fmt_secs(s: f32) -> String {
    if s < 1.0 { format!("{} ms", fr(s * 1_000.0, 0)) } else { format!("{} s", fr(s, 2)) }
}

fn note_name(note: u8) -> String {
    super::rack::note_name(note)
}

/// Couleur spectrale : sarcelle pour le grave, vert au milieu, vert pâle pour l'aigu.
fn bright_color(t: &super::theme::Theme, b: f32) -> Color {
    let low = t.select.mix(t.bg, 0.15);
    if b < 0.45 { low.mix(t.accent, b / 0.45) } else { t.accent.mix(t.accent_hot, ((b - 0.45) / 0.55).min(1.0)) }
}

/// Boucle de batterie de démonstration (deux mesures à 92 BPM), pour essayer
/// l'Atelier sans fichier.
pub fn demo_break(sample_rate: f32) -> Sample {
    let bpm = 92.0;
    let beat = 60.0 / bpm * sample_rate;
    let len = (beat * 8.0) as usize;
    let mut l = vec![0.0f32; len];
    let mut r = vec![0.0f32; len];
    let mut seed = 0x1234_5678u32;
    let mut noise = move || {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (seed >> 8) as f32 / (1u32 << 23) as f32 - 1.0
    };
    let tau = std::f32::consts::TAU;
    // (temps en croches, sorte, force)
    let hits: [(f32, u8, f32); 22] = [
        (0.0, 0, 1.0), (1.0, 2, 0.5), (2.0, 1, 0.9), (3.0, 2, 0.45), (3.5, 0, 0.7), (4.0, 0, 0.9), (5.0, 2, 0.5),
        (5.5, 1, 0.3), (6.0, 1, 0.95), (7.0, 3, 0.5), (8.0, 0, 1.0), (9.0, 2, 0.5), (10.0, 1, 0.9), (10.5, 0, 0.6),
        (11.0, 2, 0.45), (12.0, 0, 0.85), (12.5, 0, 0.6), (13.0, 2, 0.5), (14.0, 1, 1.0), (14.75, 1, 0.35),
        (15.0, 3, 0.55), (15.5, 2, 0.4),
    ];
    for (at, kind, vel) in hits {
        let start = (at * beat * 0.5) as usize;
        let dur = match kind {
            0 => 0.45,
            1 => 0.3,
            2 => 0.06,
            _ => 0.35,
        };
        let n = ((dur * sample_rate) as usize).min(len - start);
        let mut hp = 0.0f32;
        let mut phase = 0.0f32;
        for i in 0..n {
            let t = i as f32 / sample_rate;
            let (v, pan): (f32, f32) = match kind {
                0 => {
                    // Grosse caisse : sinus qui descend de 140 à 48 Hz, petit clic.
                    let f = 48.0 + 92.0 * (-t * 28.0).exp();
                    phase += f / sample_rate;
                    let click = if i < 60 { noise() * 0.3 * (1.0 - i as f32 / 60.0) } else { 0.0 };
                    ((phase * tau).sin() * (-t * 7.0).exp() + click, 0.0)
                }
                1 => {
                    // Caisse claire : corps à 190 Hz et bruit.
                    phase += 190.0 / sample_rate;
                    let body = (phase * tau).sin() * (-t * 30.0).exp() * 0.6;
                    (body + noise() * (-t * 14.0).exp() * 0.7, -0.15)
                }
                _ => {
                    // Charleston : bruit passé en passe-haut.
                    let x = noise();
                    let y = x - hp;
                    hp = x;
                    let decay = if kind == 2 { 70.0 } else { 9.0 };
                    (y * (-t * decay).exp() * 0.45, 0.3)
                }
            };
            let v = v * vel * 0.8;
            l[start + i] += v * (1.0 - pan.max(0.0));
            r[start + i] += v * (1.0 + pan.min(0.0));
        }
    }
    // Basse tenue sur les deux mesures : du grave entre les coups.
    for (i, (a, b)) in l.iter_mut().zip(r.iter_mut()).enumerate() {
        let t = i as f32 / sample_rate;
        let f = if i < len / 2 { 55.0 } else { 49.0 };
        let v = (t * f * tau).sin() * 0.12 * (0.6 + 0.4 * (t * 0.7).sin());
        *a = (*a + v).clamp(-1.0, 1.0);
        *b = (*b + v).clamp(-1.0, 1.0);
    }
    Sample { name: "Break 92".into(), sample_rate, left: l, right: r, markers: Vec::new() }
}

impl App {
    // --- Ouverture ---

    /// Ouvre un fichier dans l'Atelier.
    pub fn open_in_lab(&mut self, path: &str, source: Source) {
        if !self.ensure_sample(path) {
            return;
        }
        let sample = self.samples[path].clone();
        self.open_sample_in_lab(sample, Some(path.to_string()), source);
    }

    pub fn open_sample_in_lab(&mut self, sample: Arc<Sample>, path: Option<String>, source: Source) {
        self.lab_stop();
        let name = path
            .as_deref()
            .and_then(|p| Path::new(p).file_stem())
            .map_or_else(|| sample.name.clone(), |s| s.to_string_lossy().into_owned());
        let len = sample.len().max(1) as f64;
        let mut doc = Doc::new(sample);
        let cache = Cache::new(&doc.sample);
        doc.bpm = tempo::estimate_bpm(&cache.onsets, doc.sample.duration());
        let lab = &mut self.lab;
        lab.source = source;
        lab.path = path;
        lab.name = name;
        lab.cache = Some(cache);
        lab.undo.clear();
        lab.redo.clear();
        lab.last_edit = None;
        lab.view = (0.0, len);
        lab.shown = (0.0, len);
        lab.selected = 0;
        lab.loop_region = None;
        lab.preview = None;
        lab.grab = None;
        lab.pad_scroll = 0.0;
        lab.doc = Some(doc);
        if self.lab.doc.as_ref().is_some_and(|d| d.chop == Chop::Transients) {
            self.lab_rechop();
        }
        self.set_view(View::Lab);
    }

    /// Ouverture depuis l'appareil d'un canal (Sampler, Découpeur…).
    pub fn open_channel_in_lab(&mut self, ch: usize) {
        if let Some(path) = self.project.channels.get(ch).and_then(|c| c.instrument.sample.clone()) {
            self.open_in_lab(&path, Source::Channel(ch));
        }
    }

    /// Capture d'écran : `demo` ou un chemin de WAV ; PONZIK_LAB_MODE choisit la découpe.
    pub fn screenshot_lab(&mut self, spec: &str) {
        if spec == "demo" || spec.is_empty() {
            let sample = Arc::new(demo_break(48_000.0));
            self.open_sample_in_lab(sample, None, Source::Browser);
        } else {
            self.open_in_lab(spec, Source::Browser);
        }
        if let Some(doc) = self.lab.doc.as_mut() {
            match std::env::var("PONZIK_LAB_MODE").as_deref() {
                Ok("grid") => doc.chop = Chop::Grid,
                Ok("manual") => doc.chop = Chop::Manual,
                _ => {}
            }
            let len = doc.trim.1;
            if std::env::var_os("PONZIK_LAB_EDITS").is_some() {
                doc.fade_in = len / 40;
                doc.fade_out = len / 6;
                doc.curve_out = 0.5;
                doc.gain_db = -3.0;
                self.lab.loop_region = Some((len / 4, len / 2));
            }
        }
        self.lab_rechop();
        if let Some(doc) = self.lab.doc.as_ref() {
            let b = doc.bounds();
            let k = 2.min(b.len() - 2);
            self.lab.selected = k;
            self.lab_play(b[k], b[b.len() - 1], false);
        }
    }

    // --- État, annulation ---

    /// Point d'annulation avant une modification ; les gestes continus (même
    /// libellé à moins d'une seconde) n'en font qu'un.
    fn lab_checkpoint(&mut self, label: &str) {
        let Some(doc) = self.lab.doc.clone() else { return };
        let merge = self.lab.last_edit.as_ref().is_some_and(|(l, t)| l == label && self.time - t < 1.0);
        if !merge {
            self.lab.undo.push((label.to_string(), doc));
            if self.lab.undo.len() > 100 {
                self.lab.undo.remove(0);
            }
        }
        self.lab.redo.clear();
        self.lab.last_edit = Some((label.to_string(), self.time));
    }

    fn lab_undo(&mut self, redo: bool) -> bool {
        let (from, to) = if redo { (&mut self.lab.redo, &mut self.lab.undo) } else { (&mut self.lab.undo, &mut self.lab.redo) };
        let Some((label, doc)) = from.pop() else { return false };
        if let Some(cur) = self.lab.doc.replace(doc) {
            to.push((label.clone(), cur));
        }
        self.lab.last_edit = None;
        self.lab_refresh_cache();
        self.lab_clamp();
        self.toast(&format!("{} in the Sample Lab: {label}", if redo { "Redone" } else { "Undone" }));
        true
    }

    fn lab_refresh_cache(&mut self) {
        let Some(doc) = self.lab.doc.as_ref() else { return };
        if self.lab.cache.as_ref().is_none_or(|c| c.key != Arc::as_ptr(&doc.sample) as usize) {
            self.lab.cache = Some(Cache::new(&doc.sample));
        }
    }

    fn lab_clamp(&mut self) {
        let Some(doc) = self.lab.doc.as_ref() else { return };
        let n = doc.slices();
        self.lab.selected = self.lab.selected.min(n.saturating_sub(1));
        let len = doc.sample.len().max(1) as f64;
        let l = self.lab.view.1.clamp(16.0, len);
        self.lab.view = (self.lab.view.0.clamp(0.0, len - l), l);
    }

    /// Recalcule les coupes en mode transitoires ou grille.
    fn lab_rechop(&mut self) {
        let Some(doc) = self.lab.doc.as_mut() else { return };
        let Some(cache) = self.lab.cache.as_ref() else { return };
        let (a, b) = doc.trim;
        match doc.chop {
            Chop::Manual => {}
            Chop::Transients => {
                let found = cache.onsets.pick(&cache.mono, doc.sensitivity, MAX_SLICES);
                doc.markers = found.iter().map(|o| o.pos).filter(|&p| p > a + MIN_GAP && p + MIN_GAP < b).take(MAX_SLICES - 1).collect();
            }
            Chop::Grid => {
                let bpm = doc.bpm.unwrap_or(self.project.bpm).max(20.0);
                let per_bar = DIVISIONS[doc.division].0 as f64;
                let step = 60.0 / bpm as f64 * 4.0 / per_bar * doc.sample.sample_rate as f64;
                doc.markers = (1..MAX_SLICES)
                    .map(|k| a + (k as f64 * step).round() as usize)
                    .take_while(|&m| m + MIN_GAP < b)
                    .collect();
            }
        }
        self.lab_clamp();
    }

    /// Passe en découpe manuelle (en gardant les coupes) avant un geste à la main.
    fn lab_go_manual(&mut self) {
        if let Some(doc) = self.lab.doc.as_mut() {
            doc.chop = Chop::Manual;
        }
    }

    // --- Pré-écoute ---

    /// Tampon rendu (rognage, fondus, gain), mis en cache.
    fn lab_preview(&mut self) -> Option<Arc<Sample>> {
        let doc = self.lab.doc.as_ref()?;
        let key = doc.render_key();
        if let Some((k, s)) = &self.lab.preview
            && *k == key
        {
            return Some(s.clone());
        }
        let s = Arc::new(doc.render());
        self.lab.preview = Some((key, s.clone()));
        Some(s)
    }

    /// Joue de `from` à `to` (images du tampon de travail), en boucle ou non.
    fn lab_play(&mut self, from: usize, to: usize, looped: bool) {
        let Some(pv) = self.lab_preview() else { return };
        let Some(a) = self.lab.doc.as_ref().map(|d| d.trim.0) else { return };
        let s = from.saturating_sub(a).min(pv.len());
        let e = to.saturating_sub(a).min(pv.len());
        if e <= s + 1 {
            return;
        }
        let voice = if looped { Audition::new(pv.clone(), s, pv.len(), Some((s, e)), 1.0) } else { Audition::new(pv, s, e, None, 1.0) };
        self.send(Command::Audition(Some(Box::new(voice))));
        self.lab.play_offset = Some(a);
    }

    /// Joue la tranche `k` et la sélectionne.
    fn lab_play_slice(&mut self, k: usize) {
        let Some(b) = self.lab.doc.as_ref().map(Doc::bounds) else { return };
        if k + 1 >= b.len() {
            return;
        }
        self.lab.selected = k;
        self.anim.set(wid("lab-flash", k, 0), 1.0);
        self.lab_play(b[k], b[k + 1], false);
    }

    pub fn lab_stop(&mut self) {
        if self.lab.play_offset.take().is_some() {
            self.send(Command::Audition(None));
        }
    }

    /// Position lue, en images du tampon de travail.
    fn lab_playhead(&self) -> Option<f64> {
        let off = self.lab.play_offset?;
        self.engine.audition_position().map(|p| p + off as f64)
    }

    fn lab_toggle_play(&mut self) {
        if self.lab_playhead().is_some() {
            self.lab_stop();
            return;
        }
        let Some(doc) = self.lab.doc.as_ref() else { return };
        let b = doc.bounds();
        match self.lab.loop_region {
            Some((s, e)) => self.lab_play(s, e, true),
            None => {
                let k = self.lab.selected.min(b.len() - 2);
                self.lab_play(b[k], b[b.len() - 1], false);
            }
        }
    }

    // --- Clavier ---

    /// Raccourcis de l'Atelier ; vrai si la touche est prise.
    pub fn lab_key(&mut self, key: Key, mods: Modifiers) -> bool {
        if self.lab.doc.is_none() {
            return false;
        }
        match key {
            Key::Space if !mods.ctrl => self.lab_toggle_play(),
            Key::Char('z') if mods.ctrl => return self.lab_undo(mods.shift),
            Key::Char('y') if mods.ctrl => return self.lab_undo(true),
            Key::Left | Key::Right if !mods.ctrl => {
                let n = self.lab.doc.as_ref().map_or(1, Doc::slices);
                let k = if key == Key::Left { self.lab.selected.saturating_sub(1) } else { (self.lab.selected + 1).min(n - 1) };
                self.lab_play_slice(k);
                self.lab_reveal(k);
            }
            Key::Delete | Key::Backspace => self.lab_delete_marker(),
            Key::Escape if self.lab.loop_region.is_some() || self.lab.play_offset.is_some() => {
                self.lab.loop_region = None;
                self.lab_stop();
            }
            Key::Home => self.lab_zoom_all(),
            _ => return false,
        }
        true
    }

    /// Retire la coupe qui ouvre la tranche sélectionnée (ou la première).
    fn lab_delete_marker(&mut self) {
        let Some(doc) = self.lab.doc.as_ref() else { return };
        let b = doc.bounds();
        if b.len() <= 2 {
            return;
        }
        let k = self.lab.selected.max(1);
        let at = b[k];
        self.lab_checkpoint("Remove marker");
        self.lab_go_manual();
        if let Some(doc) = self.lab.doc.as_mut() {
            doc.markers.retain(|&m| m != at);
        }
        self.lab.selected = k - 1;
        self.lab_clamp();
    }

    fn lab_zoom_all(&mut self) {
        if let Some(doc) = self.lab.doc.as_ref() {
            self.lab.view = (0.0, doc.sample.len().max(1) as f64);
        }
    }

    /// Fait défiler la vue pour montrer la tranche `k`.
    fn lab_reveal(&mut self, k: usize) {
        let Some(b) = self.lab.doc.as_ref().map(Doc::bounds) else { return };
        let (s, e) = (b[k] as f64, b[k + 1] as f64);
        let (vs, vl) = self.lab.view;
        if s < vs || e > vs + vl {
            let len = self.lab.doc.as_ref().map_or(1.0, |d| d.sample.len() as f64);
            self.lab.view.0 = (s - vl * 0.1).clamp(0.0, (len - vl).max(0.0));
        }
    }

    // --- Opérations ---

    fn lab_replace_sample(&mut self, label: &str, edit: impl FnOnce(&mut Doc) -> Sample) {
        let Some(mut doc) = self.lab.doc.clone() else { return };
        self.lab_checkpoint(label);
        let new = edit(&mut doc);
        doc.sample = Arc::new(new);
        let len = doc.sample.len();
        doc.trim = (doc.trim.0.min(len), doc.trim.1.min(len));
        doc.markers.retain(|&m| m > 0 && m < len);
        doc.markers.sort_unstable();
        doc.markers.dedup();
        self.lab.doc = Some(doc);
        self.lab.loop_region = None;
        self.lab_refresh_cache();
        self.lab_clamp();
        self.lab.view.1 = self.lab.view.1.min(len.max(1) as f64);
        self.lab_stop();
    }

    fn lab_reverse_all(&mut self) {
        self.lab_replace_sample("Reverse", |doc| {
            let s = &doc.sample;
            let len = s.len();
            let rev = |v: &[f32]| v.iter().rev().copied().collect::<Vec<f32>>();
            doc.markers = doc.markers.iter().map(|&m| len - m).rev().collect();
            doc.trim = (len - doc.trim.1, len - doc.trim.0);
            std::mem::swap(&mut doc.fade_in, &mut doc.fade_out);
            std::mem::swap(&mut doc.curve_in, &mut doc.curve_out);
            Sample { left: rev(&s.left), right: rev(&s.right), ..(**s).clone() }
        });
        self.toast("Sample reversed");
    }

    fn lab_reverse_slice(&mut self, k: usize) {
        let Some(b) = self.lab.doc.as_ref().map(Doc::bounds) else { return };
        let (s, e) = (b[k], b[k + 1]);
        self.lab_replace_sample("Reverse slice", |doc| {
            let mut out = (*doc.sample).clone();
            out.left[s..e].reverse();
            out.right[s..e].reverse();
            out
        });
        self.lab_play_slice(k);
    }

    /// Transpose la tranche `k` comme un sampler (la durée suit la hauteur).
    fn lab_pitch_slice(&mut self, k: usize, semitones: f32) {
        let Some(b) = self.lab.doc.as_ref().map(Doc::bounds) else { return };
        let (s, e) = (b[k], b[k + 1]);
        let factor = (-semitones as f64 / 12.0).exp2();
        self.lab_replace_sample("Pitch slice", |doc| {
            let src = &doc.sample;
            let l = stretch::resample(&src.left[s..e], factor);
            let r = stretch::resample(&src.right[s..e], factor);
            let delta = l.len() as isize - (e - s) as isize;
            let shift = |m: usize| if m >= e { (m as isize + delta).max(0) as usize } else { m };
            doc.markers = doc.markers.iter().map(|&m| shift(m)).collect();
            doc.trim.1 = shift(doc.trim.1);
            let splice = |v: &[f32], mid: Vec<f32>| [&v[..s], &mid[..], &v[e..]].concat();
            Sample { left: splice(&src.left, l), right: splice(&src.right, r), ..(**src).clone() }
        });
        self.lab_play_slice(k);
    }

    /// Cale l'échantillon sur le tempo du projet : `stretch` garde la hauteur
    /// (WSOLA), sinon on change la vitesse (rééchantillonnage).
    fn lab_fit_tempo(&mut self, keep_pitch: bool) {
        let Some(from) = self.lab.doc.as_ref().and_then(|d| d.bpm) else {
            self.toast("Original tempo unknown: type it in or detect it");
            return;
        };
        let to = self.project.bpm;
        if (from - to).abs() < 0.01 {
            self.toast("Already at the project tempo");
            return;
        }
        let factor = from as f64 / to as f64;
        let label = if keep_pitch { "Stretch to tempo" } else { "Resample to tempo" };
        self.lab_replace_sample(label, |doc| {
            let s = &doc.sample;
            let (l, r) = if keep_pitch {
                stretch::wsola_stereo(&s.left, &s.right, factor, s.sample_rate)
            } else {
                (stretch::resample(&s.left, factor), stretch::resample(&s.right, factor))
            };
            let scale = |m: usize| (m as f64 * factor).round() as usize;
            doc.markers = doc.markers.iter().map(|&m| scale(m)).collect();
            doc.trim = (scale(doc.trim.0), scale(doc.trim.1).min(l.len()));
            doc.fade_in = scale(doc.fade_in);
            doc.fade_out = scale(doc.fade_out);
            doc.bpm = Some(to);
            Sample { left: l, right: r, ..(**s).clone() }
        });
        self.toast(&format!("{} → {} BPM", fr(from, 1), fr(to, 1)));
    }

    /// Transpose tout l'échantillon sans changer sa durée (WSOLA).
    fn lab_transpose(&mut self, semitones: f32) {
        self.lab_replace_sample("Transpose", |doc| {
            let s = &doc.sample;
            let (l, r) = stretch::pitch_shift(&s.left, &s.right, semitones, s.sample_rate);
            Sample { left: l, right: r, ..(**s).clone() }
        });
        self.toast(&format!("Transposed by {semitones:+} semitone{}", if semitones.abs() > 1.0 { "s" } else { "" }));
    }

    fn lab_normalize(&mut self) {
        let Some(doc) = self.lab.doc.as_ref() else { return };
        let (a, b) = doc.trim;
        let peak = doc.sample.left[a..b].iter().chain(&doc.sample.right[a..b]).fold(0.0f32, |m, v| m.max(v.abs()));
        if peak < 1e-6 {
            self.toast("Nothing to normalize: silence");
            return;
        }
        self.lab_checkpoint("Normalize");
        if let Some(doc) = self.lab.doc.as_mut() {
            doc.gain_db = (-20.0 * peak.log10()).clamp(-24.0, 24.0);
        }
        self.toast("Peak at 0 dB");
    }

    fn lab_detect_bpm(&mut self) {
        let Some(cache) = self.lab.cache.as_ref() else { return };
        let Some(doc) = self.lab.doc.as_ref() else { return };
        let sr = doc.sample.sample_rate.max(1.0);
        let dur = (doc.trim.1 - doc.trim.0) as f32 / sr;
        let bpm = tempo::estimate_bpm(&cache.onsets, doc.sample.duration()).map(|b| tempo::snap_to_loop(b, dur));
        match bpm {
            Some(b) => {
                self.lab_checkpoint("Original tempo");
                if let Some(doc) = self.lab.doc.as_mut() {
                    doc.bpm = Some(b);
                }
                self.toast(&format!("Detected tempo: {} BPM", fr(b, 1)));
                if self.lab.doc.as_ref().is_some_and(|d| d.chop == Chop::Grid) {
                    self.lab_rechop();
                }
            }
            None => self.toast("No clear rhythm to find a tempo"),
        }
    }

    // --- Vers le projet ---

    /// Écrit un échantillon dans `Échantillons/` ; renvoie son chemin.
    fn lab_write(&mut self, sample: Sample, stem: &str) -> Option<String> {
        let dir = samples_dir();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            self.toast(&format!("{} : {e}", dir.display()));
            return None;
        }
        let path = unique_path(&dir, &clean_name(stem));
        if let Err(e) = ponzik_engine::wav::write_sample(&path, &sample) {
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

    /// Rend et enregistre l'état courant ; renvoie le chemin.
    fn lab_save(&mut self) -> Option<String> {
        let doc = self.lab.doc.as_ref()?;
        let rendered = doc.render();
        let stem = format!("{} lab", base_name(&self.lab.name));
        let path = self.lab_write(rendered, &stem)?;
        self.lab.name = Path::new(&path).file_stem().map_or(stem, |s| s.to_string_lossy().into_owned());
        self.lab.path = Some(path.clone());
        Some(path)
    }

    fn lab_save_action(&mut self) {
        if let Some(path) = self.lab_save() {
            let short = Path::new(&path).file_name().map_or(String::new(), |s| s.to_string_lossy().into_owned());
            self.toast(&format!("Saved to Échantillons: {short}"));
        }
    }

    /// Enregistre et remplace l'échantillon de la source (canal ou clip).
    fn lab_apply(&mut self) {
        let Some(path) = self.lab_save() else { return };
        let has_markers = self.lab.doc.as_ref().is_some_and(|d| d.slices() > 1);
        match self.lab.source {
            Source::Channel(ch) if ch < self.project.channels.len() => {
                let inst = &mut self.project.channels[ch].instrument;
                inst.sample = Some(path);
                if inst.kind == "slicer" && has_markers {
                    inst.set(SLICER_MODE, SLICER_MARKERS);
                }
                self.focus = Focus::Channel(ch);
                self.commit("Sample Lab: replace sample");
                let name = self.project.channels[ch].name.clone();
                self.toast(&format!("\"{name}\" now plays the Sample Lab version"));
            }
            Source::Clip(i) if i < self.project.audio_clips.len() => {
                let secs = self.samples.get(&path).map_or(0.0, |s| s.duration() as f64);
                let ticks = (secs * self.project.bpm as f64 / 60.0 * PPQ as f64).round().max(24.0) as u32;
                let clip = &mut self.project.audio_clips[i];
                clip.path = path;
                clip.offset = 0.0;
                clip.len = ticks;
                self.commit("Sample Lab: replace clip");
                self.toast("Clip replaced with the Sample Lab version");
            }
            _ => self.toast("Saved to Échantillons"),
        }
    }

    /// Les tranches partent dans un nouveau Découpeur (notes 60 + k) ; avec
    /// `flip`, un pattern les rejoue dans l'ordre d'origine.
    fn lab_to_slicer(&mut self, flip: bool) {
        let Some(doc) = self.lab.doc.clone() else { return };
        let Some(path) = self.lab_save() else { return };
        let name = base_name(&self.lab.name);
        let mut def = InstrumentDef::new("slicer").with(SLICER_MODE, SLICER_MARKERS);
        def.sample = Some(path);
        let ch = self.project.add_channel(format!("{name} slices"), def, 60);
        self.focus = Focus::Channel(ch);
        let b = doc.bounds();
        let n = b.len() - 1;
        if flip {
            let sr = doc.sample.sample_rate.max(1.0) as f64;
            // Au tempo d'origine s'il est connu : le pattern garde le groove.
            let bpm = doc.bpm.unwrap_or(self.project.bpm) as f64;
            let tick = |frame: usize| ((frame - b[0]) as f64 / sr * bpm / 60.0 * PPQ as f64).round() as u32;
            let notes: Vec<Note> = (0..n)
                .map(|k| Note { start: tick(b[k]), len: (tick(b[k + 1]) - tick(b[k])).max(1), key: 60 + k as u8, vel: 0.8 })
                .collect();
            let end = tick(b[n]);
            let p = self.project.add_pattern();
            let pat = &mut self.project.patterns[p];
            pat.name = format!("Flip {name}");
            pat.len = end.div_ceil(TICKS_PER_BAR).max(1) * TICKS_PER_BAR;
            if pat.notes.len() <= ch {
                pat.notes.resize(ch + 1, Vec::new());
            }
            pat.notes[ch] = notes;
            self.select_pattern(p);
            self.commit("Flip to the rack");
            self.toast(&format!("{n} slices on {}…{} · pattern \"Flip {name}\"", note_name(60), note_name(59 + n as u8)));
        } else {
            self.commit("Slices to the rack");
            self.toast(&format!("Slicer: {n} slices on {}…{}", note_name(60), note_name(59 + n as u8)));
        }
    }

    /// Rend la tranche `k` dans un fichier (glisser vers la playlist ou le rack).
    fn lab_slice_file(&mut self, k: usize) -> Option<PathBuf> {
        let doc = self.lab.doc.as_ref()?;
        let full = doc.render();
        let b: Vec<usize> = doc.bounds().iter().map(|&m| m - doc.trim.0).collect();
        let (s, e) = (b.get(k).copied()?, b.get(k + 1).copied()?);
        let mut left = full.left[s..e].to_vec();
        let mut right = full.right[s..e].to_vec();
        // Petit fondu de sortie : la tranche ne claque pas.
        let fade = (full.sample_rate * 0.003) as usize;
        let n = left.len();
        for i in 0..fade.min(n) {
            let g = i as f32 / fade as f32;
            left[n - 1 - i] *= g;
            right[n - 1 - i] *= g;
        }
        let slice = Sample { name: String::new(), sample_rate: full.sample_rate, left, right, markers: Vec::new() };
        let stem = format!("{} slice {}", base_name(&self.lab.name), k + 1);
        self.lab_write(slice, &stem).map(PathBuf::from)
    }

    // --- Vue ---

    pub(super) fn lab_view(&mut self, c: &mut Canvas, input: &Input, header: Rect, body: Rect) {
        let t = self.theme;
        if !input.down {
            self.lab.grab = None;
        }
        // Un échantillon glissé depuis le navigateur s'ouvre ici.
        self.drop_zone(input, body, DropTarget::Lab);
        if self.lab.doc.is_none() {
            self.lab_empty(c, input, body);
            return;
        }
        self.lab_refresh_cache();

        // Vue affichée : rejoint la vue visée en douceur.
        let dt = (self.time - self.lab.last_time).clamp(0.0, 0.1);
        self.lab.last_time = self.time;
        let k = 1.0 - (-dt as f64 / T_LAYOUT as f64).exp();
        let (vs, vl) = self.lab.view;
        let (ss, sl) = self.lab.shown;
        let next = (ss + (vs - ss) * k, sl + (vl - sl) * k);
        if (next.0 - vs).abs() < 0.5 && (next.1 - vl).abs() < 0.5 {
            self.lab.shown = (vs, vl);
        } else {
            self.lab.shown = next;
            self.anim.set(wid("lab-moving", 0, 0), 0.0);
        }

        self.lab_header(c, input, header);
        let (side, main) = body.take_right(INSPECTOR_W);
        let main = Rect::new(main.x, main.y, main.w - 14.0, main.h);
        let toolbar = Rect::new(main.x, main.y, main.w, 36.0);
        let overview = Rect::new(main.x, toolbar.bottom() + 12.0, main.w, 34.0);
        let pads_h = 78.0;
        let ruler = Rect::new(main.x, overview.bottom() + 10.0, main.w, 26.0);
        let wave = Rect::new(main.x, ruler.bottom(), main.w, (main.bottom() - pads_h - 12.0 - ruler.bottom()).max(60.0));
        let pads = Rect::new(main.x, wave.bottom() + 12.0, main.w, pads_h);

        self.lab_toolbar(c, input, toolbar);
        self.lab_overview(c, input, overview);
        c.fill_rrect(Rect::new(main.x, ruler.y, main.w, ruler.h + wave.h), 12.0, t.well);
        self.lab_ruler(c, input, ruler);
        self.lab_wave(c, input, wave);
        self.lab_pads(c, input, pads);
        self.lab_inspector(c, input, side);
    }

    fn lab_empty(&mut self, c: &mut Canvas, input: &Input, body: Rect) {
        let t = self.theme;
        let r = body.inset(4.0);
        c.fill_rrect(r, 16.0, t.panel);
        let dash = t.hairline.alpha(2.2);
        c.stroke_rrect(r.inset(10.0), 12.0, 1.0, dash);
        let (cx, cy) = r.center();
        // Forme d'onde décorative, coupée en tranches colorées.
        let w = 360.0f32.min(r.w - 80.0);
        for i in 0..72 {
            let x = cx - w * 0.5 + i as f32 * w / 72.0;
            let k = (i / 12) as usize;
            let h = 6.0 + 34.0 * ((i as f32 * 0.7).sin() * 0.5 + 0.5) * (-((i % 12) as f32) / 6.0).exp();
            c.fill_rrect(Rect::new(x, cy - 40.0 - h * 0.5, 2.5, h), 1.0, pattern_color(k).alpha(0.75));
        }
        for k in 1..6 {
            let x = cx - w * 0.5 + k as f32 * w / 6.0 - 2.0;
            c.fill_rect(Rect::new(x, cy - 72.0, 1.5, 64.0), t.text.alpha(0.5));
        }
        self.text_center(c, self.f.sans_med, 16.0, cx, cy + 10.0, "Drop a sample here", t.text);
        self.text_center(c, self.f.sans, 13.0, cx, cy + 34.0, "or click a WAV in the browser · double-click an audio clip", t.text2);
        let label = "Try a drum loop";
        let bw = self.fonts.measure(self.f.sans_med, 13.0, label) + 52.0;
        let btn = Rect::new(cx - bw * 0.5, cy + 60.0, bw, 36.0);
        if self.lab_button(c, input, wid("lab-demo", 0, 0), btn, Some(Icon::Play), label, 1, "Opens a demo drum loop") {
            let sample = Arc::new(demo_break(self.sample_rate as f32));
            self.open_sample_in_lab(sample, None, Source::Browser);
        }
    }

    /// Bouton pilule : icône et libellé. `style` : 0 normal, 1 principal, 2 enfoncé.
    fn lab_button(&mut self, c: &mut Canvas, input: &Input, id: u64, r: Rect, icon: Option<Icon>, label: &str, style: u8, tip: &str) -> bool {
        let t = self.theme;
        let hov = self.hovered(input, id, r);
        let hv = self.hover_anim(id, hov);
        let press = self.anim.to(id ^ 0x5052, if hov && input.down { 0.96 } else { 1.0 }, T_PRESS);
        let rr = r.scale(press);
        let (fill, fg) = match style {
            1 => (t.accent.mix(t.accent_hot, hv * 0.2), t.on_accent),
            2 => (t.raised_hi.mix(t.accent, 0.18), t.accent),
            _ => (t.raised.mix(t.raised_hi, hv), t.text2.mix(t.text, hv)),
        };
        c.fill_rrect(rr, 10.0, fill);
        if style != 1 {
            c.stroke_rrect(rr, 10.0, 1.0, t.hairline);
        }
        let mut x = rr.x + 14.0;
        if let Some(icon) = icon {
            draw_icon(c, icon, x + 7.0, rr.y + rr.h * 0.5, 15.0 * press, fg);
            x += 22.0;
        }
        let max = rr.right() - 10.0 - x;
        self.text_fit(c, self.f.sans_med, 13.0, x, rr.y + rr.h * 0.5, label, max, fg);
        if hov && !tip.is_empty() {
            self.set_tip(tip, "");
        }
        hov && input.pressed
    }

    fn button_width(&self, icon: bool, label: &str) -> f32 {
        self.fonts.measure(self.f.sans_med, 13.0, label) + if icon { 50.0 } else { 28.0 }
    }

    /// Curseur horizontal : libellé, valeur, piste. Renvoie la nouvelle position (0 à 1).
    fn lab_slider(&mut self, c: &mut Canvas, input: &Input, id: u64, r: Rect, label: &str, norm: f32, text: &str, default: f32, tip: &str) -> Option<f32> {
        let t = self.theme;
        let track = Rect::new(r.x, r.y + 26.0, r.w, 4.0);
        let hit = Rect::new(r.x - 4.0, r.y, r.w + 8.0, r.h);
        let hov = self.hovered(input, id, hit);
        let mut out = None;
        if hov && input.pressed {
            if input.double {
                out = Some(default);
            } else {
                self.begin_drag(id, input, norm);
            }
        }
        if self.is_dragging(id) {
            out = Some(((input.x - track.x) / track.w).clamp(0.0, 1.0));
        }
        if hov && input.wheel != 0.0 {
            let step = if input.mods.shift { 0.005 } else { 0.03 };
            out = Some((norm + input.wheel * step).clamp(0.0, 1.0));
        }
        let hv = self.hover_anim(id, hov || self.is_dragging(id));
        let shown = self.anim.to(id ^ 0x534c, out.unwrap_or(norm), T_PRESS);
        self.text(c, self.f.sans, 12.0, r.x, r.y + 9.0, label, t.text2.mix(t.text, hv));
        self.text_right(c, self.f.mono, 11.0, r.right(), r.y + 9.0, text, t.muted.mix(t.text, hv));
        c.fill_rrect(track, 2.0, t.raised_hi);
        c.fill_rrect(Rect::new(track.x, track.y, (track.w * shown).max(4.0), track.h), 2.0, t.accent.mix(t.accent_hot, hv * 0.2));
        let kx = track.x + track.w * shown;
        c.fill_circle(kx, track.y + 2.0, 6.0 + hv, t.text);
        c.fill_circle(kx, track.y + 2.0, 2.5, t.accent_deep);
        if hov {
            self.set_tip(&format!("{tip}\nDrag · wheel (Shift: fine) · double-click: default"), "");
        }
        out
    }

    fn lab_header(&mut self, c: &mut Canvas, input: &Input, r: Rect) {
        let t = self.theme;
        let Some(doc) = self.lab.doc.as_ref() else { return };
        let s = &doc.sample;
        let secs = (doc.trim.1 - doc.trim.0) as f32 / s.sample_rate.max(1.0);
        let chans = if s.is_mono() { "mono" } else { "stereo" };
        let mut meta = format!("{} · {} kHz · {chans}", fmt_secs(secs), fr(s.sample_rate / 1_000.0, 1));
        if let Some(b) = doc.bpm {
            meta.push_str(&format!(" · {} BPM", fr(b, 1)));
        }
        let slices = doc.slices();
        let source = self.lab.source;

        // Actions, de droite à gauche.
        let mut x = r.right();
        let gap = 8.0;
        let send = "To Slicer";
        let w = self.button_width(true, send);
        x -= w;
        let tip = format!("New Slicer channel: slice k plays on note {} + k", note_name(60));
        if self.lab_button(c, input, wid("lab-send", 0, 0), Rect::new(x, r.y, w, r.h), Some(Icon::Slice), send, 1, &tip) {
            self.lab_to_slicer(false);
        }
        let flip = "Flip";
        let w = self.button_width(true, flip);
        x -= w + gap;
        let tip = "Slicer plus a pattern replaying the slices in their original order\nRearrange the notes in the piano roll to flip the sample";
        if self.lab_button(c, input, wid("lab-flip", 0, 0), Rect::new(x, r.y, w, r.h), Some(Icon::Piano), flip, 0, tip) {
            self.lab_to_slicer(true);
        }
        let target = match source {
            Source::Channel(ch) => self.project.channels.get(ch).map(|c| c.name.clone()),
            Source::Clip(i) => self.project.audio_clips.get(i).map(|_| "the clip".to_string()),
            Source::Browser => None,
        };
        if let Some(name) = target {
            let label = "Apply";
            let w = self.button_width(true, label);
            x -= w + gap;
            let tip = format!("Saves and replaces the sample of \"{name}\"\nCtrl+Z brings the old one back");
            if self.lab_button(c, input, wid("lab-apply", 0, 0), Rect::new(x, r.y, w, r.h), Some(Icon::Check), label, 0, &tip) {
                self.lab_apply();
            }
        }
        let save = Rect::new(x - gap - 36.0, r.y, 36.0, r.h);
        if self.icon_button(c, input, wid("lab-save", 0, 0), save, Icon::Save, false, ("Save to Échantillons (24-bit WAV, slice markers included)", "")) {
            self.lab_save_action();
        }
        x = save.x;

        // Nom et détails.
        let name = self.lab.name.clone();
        let left = r.x + 4.0;
        let avail = x - 16.0 - left;
        self.text_fit(c, self.f.sans_med, 15.0, left, r.y + 10.0, &name, avail, t.text);
        let detail = format!("{meta} · {slices} slice{}", if slices > 1 { "s" } else { "" });
        self.text_fit(c, self.f.sans, 12.0, left, r.y + 28.0, &detail, avail, t.muted);
    }

    fn lab_toolbar(&mut self, c: &mut Canvas, input: &Input, r: Rect) {
        let t = self.theme;
        let Some(doc) = self.lab.doc.as_ref() else { return };
        let chop = doc.chop;
        let sel = match chop {
            Chop::Transients => 0,
            Chop::Grid => 1,
            Chop::Manual => 2,
        };
        let modes = Rect::new(r.x, r.y, 318.0, r.h);
        let tips = [
            ("Chop at transients\nOne slice per hit; sensitivity sets how many", ""),
            ("Chop to the grid\nEven slices at the original tempo", ""),
            ("Chop by hand\nClick: add · drag: move · hold right-click: erase", ""),
        ];
        if let Some(i) = self.segmented_tips(c, input, wid("lab-chop", 0, 0), modes, &["Transients", "Grid", "Manual"], &tips, sel) {
            self.lab_checkpoint("Chop mode");
            if let Some(doc) = self.lab.doc.as_mut() {
                doc.chop = [Chop::Transients, Chop::Grid, Chop::Manual][i];
            }
            if i == 1 && self.lab.doc.as_ref().is_some_and(|d| d.bpm.is_none()) {
                let bpm = self.project.bpm;
                if let Some(doc) = self.lab.doc.as_mut() {
                    doc.bpm = Some(bpm);
                }
            }
            self.lab_rechop();
        }
        let x = modes.right() + 16.0;
        let Some(doc) = self.lab.doc.as_ref() else { return };
        let (sens, division, bpm) = (doc.sensitivity, doc.division, doc.bpm);
        match chop {
            Chop::Transients => {
                let sl = Rect::new(x, r.y + 2.0, 220.0f32.min(r.right() - x - 120.0), 34.0);
                let text = format!("{} %", fr(sens * 100.0, 0));
                if let Some(v) = self.lab_slider(c, input, wid("lab-sens", 0, 0), sl, "Sensitivity", sens, &text, 0.5, "Hit sensitivity: higher means more slices") {
                    self.lab_checkpoint("Sensitivity");
                    if let Some(doc) = self.lab.doc.as_mut() {
                        doc.sensitivity = v;
                    }
                    self.lab_rechop();
                }
            }
            Chop::Grid => {
                let seg = Rect::new(x, r.y, 220.0, r.h);
                let labels: Vec<&str> = DIVISIONS.iter().map(|d| d.1).collect();
                let tips = [("Quarter notes", ""), ("Eighth notes", ""), ("Sixteenth notes", ""), ("Thirty-second notes", "")];
                if let Some(i) = self.segmented_tips(c, input, wid("lab-div", 0, 0), seg, &labels, &tips, division) {
                    self.lab_checkpoint("Grid");
                    if let Some(doc) = self.lab.doc.as_mut() {
                        doc.division = i;
                    }
                    self.lab_rechop();
                }
                let field = Rect::new(seg.right() + 10.0, r.y, 128.0, r.h);
                let v = bpm.unwrap_or(self.project.bpm);
                if let Some(nv) = self.drag_field(c, input, wid("lab-grid-bpm", 0, 0), field, v, 0.1, 1.0, &fr(v, 1), "BPM") {
                    let nv = (nv * 10.0).round() / 10.0;
                    self.lab_checkpoint("Original tempo");
                    if let Some(doc) = self.lab.doc.as_mut() {
                        doc.bpm = Some(nv.clamp(30.0, 300.0));
                    }
                    self.lab_rechop();
                }
                self.tip_zone(input, field, "Original tempo of the sample\nDrag vertically · wheel", "");
            }
            Chop::Manual => {
                let cy = r.y + r.h * 0.5;
                draw_icon(c, Icon::Info, x + 8.0, cy, 14.0, t.muted);
                let line = "Click: add a marker · drag: move · hold right-click: erase";
                let max = r.right() - x - 140.0;
                self.text_fit(c, self.f.sans, 12.0, x + 24.0, cy, line, max, t.text2);
            }
        }
        // À droite : couleurs spectrales et vue entière.
        let all = Rect::new(r.right() - 34.0, r.y + 1.0, 34.0, 34.0);
        if self.icon_button(c, input, wid("lab-all", 0, 0), all, Icon::Columns, false, ("Show all", "Home")) {
            self.lab_zoom_all();
        }
        let spec = Rect::new(all.x - 40.0, all.y, 34.0, 34.0);
        let on = self.lab.spectral;
        let tip = if on { "Spectral colors: lows in teal, highs in pale green\nClick: slice colors" } else { "Slice colors\nClick: spectral colors (brightness of the sound)" };
        if self.icon_button(c, input, wid("lab-spec", 0, 0), spec, Icon::Spectral, on, (tip, "")) {
            self.lab.spectral = !on;
        }
    }

    /// Vue d'ensemble : tout l'échantillon, la fenêtre visible à tirer.
    fn lab_overview(&mut self, c: &mut Canvas, input: &Input, r: Rect) {
        let t = self.theme;
        let Some(doc) = self.lab.doc.as_ref() else { return };
        let Some(cache) = self.lab.cache.as_ref() else { return };
        let len = doc.sample.len().max(1) as f64;
        c.fill_rrect(r, 10.0, t.well);
        let g = r.inset_xy(6.0, 4.0);
        let mid = g.y + g.h * 0.5;
        let n = cache.peaks.len().max(1);
        let bounds = doc.bounds();
        for px in 0..g.w as usize {
            let a = px * n / g.w as usize;
            let b = ((px + 1) * n / g.w as usize).max(a + 1).min(n);
            let (lo, hi) = cache.peaks[a.min(n - 1)..b].iter().fold((0.0f32, 0.0f32), |(l, h), &(x, y)| (l.min(x), h.max(y)));
            let frame = a * BUCKET;
            let inside = frame >= doc.trim.0 && frame < doc.trim.1;
            let k = bounds.partition_point(|&m| m <= frame).saturating_sub(1);
            let col = if inside { pattern_color(k).alpha(0.7) } else { t.muted.alpha(0.3) };
            c.fill_rect(Rect::new(g.x + px as f32, mid - hi * g.h * 0.5, 1.0, ((hi - lo) * g.h * 0.5).max(1.0)), col);
        }
        let (vs, vl) = self.lab.shown;
        let vx = g.x + (vs / len) as f32 * g.w;
        let vw = ((vl / len) as f32 * g.w).max(6.0);
        let win = Rect::new(vx, r.y + 1.0, vw, r.h - 2.0);
        let id = wid("lab-ov", 0, 0);
        let hov = self.hovered(input, id, r);
        let hv = self.hover_anim(id, hov || self.is_dragging(id));
        if vl < len - 1.0 {
            c.fill_rrect(win, 8.0, t.select.alpha(0.08 + 0.06 * hv));
            c.stroke_rrect(win, 8.0, 1.0, t.select.alpha(0.6 + 0.3 * hv));
        }
        if let Some(p) = self.lab_playhead() {
            let x = g.x + (p / len) as f32 * g.w;
            c.fill_rect(Rect::new(x.round(), r.y + 2.0, 1.0, r.h - 4.0), t.accent);
        }
        if hov && input.pressed {
            self.begin_drag(id, input, 0.0);
            self.lab.grab = Some(Grab::Overview);
        }
        if self.is_dragging(id) && self.lab.grab == Some(Grab::Overview) {
            let center = ((input.x - g.x) / g.w).clamp(0.0, 1.0) as f64 * len;
            let vl = self.lab.view.1;
            self.lab.view.0 = (center - vl * 0.5).clamp(0.0, (len - vl).max(0.0));
            self.lab.shown.0 = self.lab.view.0;
        }
        if hov {
            self.set_tip("Overview\nClick or drag: jump there", "");
        }
    }

    /// Règle : temps ou mesures ; balayer pour écouter, tirer la boucle en haut.
    fn lab_ruler(&mut self, c: &mut Canvas, input: &Input, r: Rect) {
        let t = self.theme;
        let Some(doc) = self.lab.doc.as_ref() else { return };
        let sr = doc.sample.sample_rate.max(1.0) as f64;
        let len = doc.sample.len().max(1) as f64;
        let origin = doc.trim.0 as f64;
        let bpm = doc.bpm;
        let bounds = doc.bounds();
        let (vs, vl) = self.lab.shown;
        let x_of = |f: f64| r.x + ((f - vs) / vl) as f32 * r.w;
        let f_at = |x: f32| (vs + ((x - r.x) / r.w) as f64 * vl).clamp(0.0, len);
        let prev = c.push_clip(r);
        c.fill_rect(Rect::new(r.x, r.bottom() - 1.0, r.w, 1.0), t.hairline);
        // Graduations.
        let px_per_sec = r.w as f64 / (vl / sr);
        match bpm {
            Some(bpm) => {
                let beat = 60.0 / bpm as f64 * sr;
                let per_beat = beat / vl * r.w as f64;
                let step_beats = [1.0, 2.0, 4.0, 8.0, 16.0, 32.0].into_iter().find(|s| s * per_beat >= 14.0).unwrap_or(64.0);
                let first = ((vs - origin) / beat / step_beats).floor() as i64;
                let last = ((vs + vl - origin) / beat / step_beats).ceil() as i64;
                for i in first..=last {
                    let beat_n = i as f64 * step_beats;
                    let x = x_of(origin + beat_n * beat);
                    let bar = beat_n.rem_euclid(4.0) == 0.0;
                    let h = if bar { 10.0 } else { 5.0 };
                    c.fill_rect(Rect::new(x.round(), r.bottom() - h, 1.0, h), t.text.alpha(if bar { 0.35 } else { 0.18 }));
                    if bar && (per_beat * 4.0 >= 34.0 || (beat_n / 4.0).rem_euclid(4.0) == 0.0) {
                        let label = format!("{}", (beat_n / 4.0) as i64 + 1);
                        self.text(c, self.f.mono, 11.0, x + 4.0, r.y + 15.0, &label, t.muted);
                    }
                }
            }
            None => {
                let step = [0.001, 0.002, 0.005, 0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0]
                    .into_iter()
                    .find(|s| s * px_per_sec >= 70.0)
                    .unwrap_or(120.0);
                let first = (vs / sr / step).floor() as i64;
                let last = ((vs + vl) / sr / step).ceil() as i64;
                for i in first..=last {
                    let secs = i as f64 * step;
                    let x = x_of(secs * sr);
                    c.fill_rect(Rect::new(x.round(), r.bottom() - 8.0, 1.0, 8.0), t.text.alpha(0.3));
                    let label = if step < 1.0 { format!("{} s", fr(secs as f32, if step < 0.01 { 3 } else if step < 0.1 { 2 } else { 1 })) } else { format!("{} s", secs as i64) };
                    self.text(c, self.f.mono, 11.0, x + 4.0, r.y + 15.0, &label, t.muted);
                }
            }
        }
        // Boucle.
        let band = Rect::new(r.x, r.y, r.w, 9.0);
        if let Some((a, b)) = self.lab.loop_region {
            let (xa, xb) = (x_of(a as f64), x_of(b as f64));
            c.fill_rrect(Rect::new(xa, band.y + 1.0, xb - xa, 6.0), 3.0, t.select.alpha(0.85));
        }
        // Tête de lecture.
        if let Some(p) = self.lab_playhead() {
            let x = x_of(p);
            let mut tri = Shape::new();
            tri.polygon(&[(x - 5.0, r.bottom() - 9.0), (x + 5.0, r.bottom() - 9.0), (x, r.bottom() - 2.0)]);
            c.fill_path(&tri, t.select);
        }
        c.set_clip(Some(prev));

        let snap = |f: f64| -> usize {
            // Aimanté aux bornes de tranches à 6 px près.
            let near = bounds.iter().copied().min_by(|&a, &b| (a as f64 - f).abs().total_cmp(&(b as f64 - f).abs()));
            match near {
                Some(m) if ((m as f64 - f) / vl * r.w as f64).abs() < 6.0 => m,
                _ => f as usize,
            }
        };
        let id = wid("lab-ruler", 0, 0);
        let hov = self.hovered(input, id, r);
        let on_band = input.y < band.bottom() + 3.0;
        if hov {
            if on_band {
                self.set_tip("Loop: drag to draw it (snaps to slices)\nSpace plays it · right-click: remove", "");
            } else {
                self.set_tip("Click: play from here · drag: scrub", "Space");
            }
        }
        if hov && input.right_pressed && on_band {
            self.lab.loop_region = None;
        }
        if hov && input.pressed {
            self.begin_drag(id, input, 0.0);
            let f = f_at(input.x);
            if on_band {
                self.lab.grab = Some(Grab::Loop(snap(f)));
            } else {
                self.lab.grab = Some(Grab::Scrub(input.x));
                self.lab_play(f as usize, usize::MAX, false);
            }
        }
        if self.is_dragging(id) {
            match self.lab.grab {
                Some(Grab::Loop(anchor)) => {
                    let f = snap(f_at(input.x));
                    let (a, b) = (anchor.min(f), anchor.max(f));
                    self.lab.loop_region = (b > a + MIN_GAP).then_some((a, b));
                    if input.released && let Some((a, b)) = self.lab.loop_region {
                        self.lab_play(a, b, true);
                    }
                }
                Some(Grab::Scrub(last)) if (input.x - last).abs() >= 3.0 => {
                    self.lab.grab = Some(Grab::Scrub(input.x));
                    self.lab_play(f_at(input.x) as usize, usize::MAX, false);
                }
                _ => {}
            }
        }
    }

    /// Forme d'onde : tranches, coupes, rognage, fondus, tête de lecture.
    fn lab_wave(&mut self, c: &mut Canvas, input: &Input, r: Rect) {
        let t = self.theme;
        let Some(doc) = self.lab.doc.clone() else { return };
        let len = doc.sample.len().max(1) as f64;
        let (vs, vl) = self.lab.shown;
        let x_of = |f: f64| r.x + ((f - vs) / vl) as f32 * r.w;
        let f_at = |x: f32| (vs + ((x - r.x) / r.w) as f64 * vl).clamp(0.0, len);
        let bounds = doc.bounds();
        let (ta, tb) = doc.trim;
        let selected = self.lab.selected.min(bounds.len() - 2);
        let spectral = self.lab.spectral;
        let prev = c.push_clip(r);

        // Fonds des tranches.
        for k in 0..bounds.len() - 1 {
            let (xa, xb) = (x_of(bounds[k] as f64), x_of(bounds[k + 1] as f64));
            if xb < r.x || xa > r.right() {
                continue;
            }
            let flash = self.anim.to(wid("lab-flash", k, 0), 0.0, 0.25);
            let sel = self.anim.to(wid("lab-sel", k, 0), (k == selected) as u8 as f32, T_HOVER);
            let a = 0.035 + 0.07 * sel + 0.14 * flash + if k % 2 == 1 { 0.02 } else { 0.0 };
            c.fill_rect(Rect::new(xa, r.y, xb - xa, r.h), pattern_color(k).alpha(a));
        }
        // Zones rognées : assombries.
        for (a, b) in [(0.0, ta as f64), (tb as f64, len)] {
            let (xa, xb) = (x_of(a).max(r.x), x_of(b).min(r.right()));
            if xb > xa {
                c.fill_rect(Rect::new(xa, r.y, xb - xa, r.h), t.bg_deep.alpha(0.55));
            }
        }
        let mid = r.y + r.h * 0.5;
        c.fill_rect(Rect::new(r.x, mid.round(), r.w, 1.0), t.hairline);

        // Forme d'onde, à l'enveloppe (gain et fondus) : ce que l'on entend.
        let fpp = vl / r.w as f64;
        let amp = r.h * 0.46;
        let Some(cache) = self.lab.cache.as_ref() else { return };
        let s = &doc.sample;
        let n_peaks = cache.peaks.len();
        let x0 = x_of(0.0).max(r.x) as i32;
        let x1 = x_of(len).min(r.right()) as i32;
        if fpp < 1.5 {
            // Très près : on relie les échantillons.
            let mut pts = Vec::new();
            let first = f_at(r.x).floor().max(0.0) as usize;
            let last = (f_at(r.right()).ceil() as usize + 1).min(s.len());
            for i in first..last {
                let v = 0.5 * (s.left[i] + s.right[i]) * doc.envelope(i).max(if i < ta || i >= tb { 1.0 } else { 0.0 });
                pts.push((x_of(i as f64), mid - v * amp));
            }
            let col = if spectral { bright_color(&t, cache.bright.get(first / BUCKET).copied().unwrap_or(0.5)) } else { t.accent };
            c.stroke_polyline(&pts, 1.5, col);
            if fpp < 0.2 {
                for &(x, y) in &pts {
                    c.fill_circle(x, y, 2.0, col);
                }
            }
        } else {
            for px in x0..x1 {
                let fa = f_at(px as f32);
                let fb = f_at(px as f32 + 1.0);
                let (mut lo, mut hi, mut br) = (0.0f32, 0.0f32, 0.0f32);
                if fpp >= BUCKET as f64 {
                    let (ba, bb) = ((fa as usize / BUCKET).min(n_peaks.saturating_sub(1)), (fb as usize / BUCKET).clamp(fa as usize / BUCKET + 1, n_peaks));
                    for j in ba..bb {
                        let (l, h) = cache.peaks[j];
                        lo = lo.min(l);
                        hi = hi.max(h);
                        br = br.max(cache.bright[j]);
                    }
                } else {
                    let (ia, ib) = (fa as usize, (fb as usize).max(fa as usize + 1).min(s.len()));
                    for i in ia..ib {
                        lo = lo.min(s.left[i].min(s.right[i]));
                        hi = hi.max(s.left[i].max(s.right[i]));
                    }
                    br = cache.bright.get(ia / BUCKET).copied().unwrap_or(0.0);
                }
                let frame = fa as usize;
                let inside = frame >= ta && frame < tb;
                let x = px as f32;
                if !inside {
                    c.fill_rect(Rect::new(x, mid - hi * amp, 1.0, ((hi - lo) * amp).max(1.0)), t.muted.alpha(0.28));
                    continue;
                }
                let e = doc.envelope(frame);
                if e < 0.999 {
                    // Fantôme de l'original sous l'enveloppe.
                    c.fill_rect(Rect::new(x, mid - hi * amp, 1.0, ((hi - lo) * amp).max(1.0)), t.text.alpha(0.07));
                }
                let (l, h) = (lo * e, hi * e);
                let k = bounds.partition_point(|&m| m <= frame).saturating_sub(1);
                let col = if spectral { bright_color(&t, br) } else { pattern_color(k) };
                let col = col.alpha(if k == selected { 0.95 } else { 0.78 });
                c.fill_rect(Rect::new(x, mid - h.min(1.0) * amp, 1.0, ((h.min(1.0) - l.max(-1.0)) * amp).max(1.0)), col);
                if h > 1.0 || l < -1.0 {
                    // Écrêtage après le gain : en rouge.
                    c.fill_rect(Rect::new(x, mid - amp - 2.0, 1.0, 3.0), t.negative);
                    c.fill_rect(Rect::new(x, mid + amp - 1.0, 1.0, 3.0), t.negative);
                }
            }
        }

        // Fonction d'attaque et seuil, sous la forme d'onde (mode transitoires).
        if doc.chop == Chop::Transients {
            let base = r.bottom() - 4.0;
            let hgt = 30.0;
            let hop = onset::HOP as f64;
            let mut curve = Vec::new();
            let mut thresh = Vec::new();
            let mut px = x0 as f32;
            while px < x1 as f32 {
                let (fa, fb) = (f_at(px) / hop, f_at(px + 2.0) / hop);
                let (ka, kb) = (fa as usize, (fb as usize).max(fa as usize + 1).min(cache.onsets.flux.len()));
                let f = cache.onsets.flux.get(ka..kb).map_or(0.0, |w| w.iter().fold(0.0f32, |m, &v| m.max(v)));
                curve.push((px, base - f.min(1.0) * hgt));
                thresh.push((px, base - cache.onsets.threshold(ka, doc.sensitivity).min(1.0) * hgt));
                px += 2.0;
            }
            if curve.len() > 1 {
                let mut fill = curve.clone();
                fill.push((curve[curve.len() - 1].0, base));
                fill.push((curve[0].0, base));
                let mut path = Shape::new();
                path.polygon(&fill);
                c.fill_path(&path, t.select.alpha(0.12));
                c.stroke_polyline(&curve, 1.0, t.select.alpha(0.55));
                c.stroke_polyline(&thresh, 1.0, t.text.alpha(0.3));
            }
        }

        // Courbes de fondu.
        let fade_line = |c: &mut Canvas, from: usize, to: usize| {
            let mut pts = Vec::new();
            let steps = 48;
            for i in 0..=steps {
                let f = from + (to - from) * i / steps;
                let e = doc.envelope(f.min(tb.saturating_sub(1))) / db_to_gain(doc.gain_db).max(1e-6);
                pts.push((x_of(f as f64), r.bottom() - 2.0 - e.min(1.0) * (r.h - 4.0)));
            }
            c.stroke_polyline(&pts, 1.5, t.text.alpha(0.75));
        };
        if doc.fade_in > 0 {
            fade_line(c, ta, ta + doc.fade_in);
        }
        if doc.fade_out > 0 {
            fade_line(c, tb - doc.fade_out, tb);
        }

        // Coupes : trait et fanion numéroté de la tranche qui commence.
        for (k, &m) in bounds.iter().enumerate().take(bounds.len() - 1).skip(1) {
            let x = x_of(m as f64).round();
            if x < r.x - 20.0 || x > r.right() + 20.0 {
                continue;
            }
            let col = pattern_color(k);
            let hv = self.anim.get(wid("lab-mk-h", m, 0)).unwrap_or(0.0);
            c.fill_rect(Rect::new(x - 0.5, r.y, 1.5 + hv, r.h), col.alpha(0.65 + 0.35 * hv));
        }
        for (k, &m) in bounds.iter().enumerate().take(bounds.len() - 1) {
            let x = x_of(m as f64).round();
            if x < r.x - 30.0 || x > r.right() {
                continue;
            }
            let col = pattern_color(k);
            let label = format!("{}", k + 1);
            let w = self.fonts.measure(self.f.mono_med, 11.0, &label) + 10.0;
            let sel = k == selected;
            // Tranche trop étroite pour son numéro : un simple fanion (sauf la sélection).
            let room = x_of(bounds[k + 1] as f64) - x;
            let flag = if room >= w + 2.0 || sel { Rect::new(x, r.y + 4.0, w, 16.0) } else { Rect::new(x, r.y + 4.0, room.clamp(3.0, 6.0), 10.0) };
            c.fill_rrect(flag, if flag.w < 8.0 { 1.5 } else { 4.0 }, if sel { col } else { col.mix(t.bg, 0.45) });
            if flag.w >= w {
                self.text(c, self.f.mono_med, 11.0, flag.x + 5.0, flag.y + 8.0, &label, if sel { t.on_accent } else { t.text });
            }
        }

        // Poignées : fondus en haut, rognage en bas.
        let handles = [
            (Grab::FadeIn, x_of((ta + doc.fade_in) as f64), r.y + 26.0),
            (Grab::FadeOut, x_of((tb - doc.fade_out) as f64), r.y + 26.0),
            (Grab::TrimStart, x_of(ta as f64), r.bottom() - 12.0),
            (Grab::TrimEnd, x_of(tb as f64), r.bottom() - 12.0),
        ];
        let curve_handles = [
            (Grab::CurveIn, doc.fade_in > 0, ta + doc.fade_in / 2),
            (Grab::CurveOut, doc.fade_out > 0, tb - doc.fade_out / 2),
        ];
        let wave_id = wid("lab-wave", 0, 0);
        let hov_wave = self.hovered(input, wave_id, r);
        let mut over_handle = None;
        if hov_wave && self.lab.grab.is_none() {
            for (g, x, y) in handles {
                if (input.x - x).abs() <= HANDLE + 2.0 && (input.y - y).abs() <= HANDLE + 4.0 {
                    over_handle = Some(g);
                }
            }
            for (g, on, f) in curve_handles {
                let e = doc.envelope(f) / db_to_gain(doc.gain_db).max(1e-6);
                let (x, y) = (x_of(f as f64), r.bottom() - 2.0 - e * (r.h - 4.0));
                if on && (input.x - x).abs() <= HANDLE && (input.y - y).abs() <= HANDLE {
                    over_handle = Some(g);
                }
            }
        }
        for (g, x, y) in handles {
            let hv = self.hover_anim(wid("lab-h", grab_key(g), 0), over_handle == Some(g) || self.lab.grab == Some(g));
            let trim = matches!(g, Grab::TrimStart | Grab::TrimEnd);
            if trim {
                // Crochet de rognage.
                let dir = if g == Grab::TrimStart { 1.0 } else { -1.0 };
                c.fill_rect(Rect::new(x - 1.0, r.y, 2.0, r.h), t.text.alpha(0.25 + 0.5 * hv));
                c.stroke_polyline(&[(x + dir * 7.0, y - 9.0), (x, y - 9.0), (x, y + 6.0), (x + dir * 7.0, y + 6.0)], 2.0, t.text.alpha(0.6 + 0.4 * hv));
            } else {
                let s = 5.0 + 1.5 * hv;
                c.fill_rrect(Rect::new(x - s, y - s, 2.0 * s, 2.0 * s), 2.0, t.text.alpha(0.55 + 0.45 * hv));
                c.stroke_rrect(Rect::new(x - s, y - s, 2.0 * s, 2.0 * s), 2.0, 1.0, t.bg);
            }
        }
        for (g, on, f) in curve_handles {
            if !on {
                continue;
            }
            let e = doc.envelope(f) / db_to_gain(doc.gain_db).max(1e-6);
            let (x, y) = (x_of(f as f64), r.bottom() - 2.0 - e * (r.h - 4.0));
            let hv = self.hover_anim(wid("lab-h", grab_key(g), 0), over_handle == Some(g) || self.lab.grab == Some(g));
            c.fill_circle(x, y, 4.0 + 1.5 * hv, t.bg);
            c.stroke_circle(x, y, 4.0 + 1.5 * hv, 1.5, t.text.alpha(0.6 + 0.4 * hv));
        }

        // Boucle et tête de lecture.
        if let Some((a, b)) = self.lab.loop_region {
            let (xa, xb) = (x_of(a as f64), x_of(b as f64));
            c.fill_rect(Rect::new(xa, r.y, xb - xa, r.h), t.select.alpha(0.06));
            c.fill_rect(Rect::new(xa, r.y, 1.0, r.h), t.select.alpha(0.6));
            c.fill_rect(Rect::new(xb - 1.0, r.y, 1.0, r.h), t.select.alpha(0.6));
        }
        if let Some(p) = self.lab_playhead() {
            let x = x_of(p).round();
            c.fill_rect(Rect::new(x - 3.0, r.y, 6.0, r.h), t.select.alpha(0.12));
            c.fill_rect(Rect::new(x - 1.0, r.y, 2.0, r.h), t.select);
        }
        c.set_clip(Some(prev));

        // --- Gestes ---
        let marker_near = |x: f32| -> Option<usize> {
            doc.markers
                .iter()
                .enumerate()
                .filter(|(_, m)| **m > ta && **m < tb)
                .map(|(i, &m)| (i, (x_of(m as f64) - x).abs()))
                .filter(|(_, d)| *d <= 5.0)
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(i, _)| i)
        };
        let hover_marker = if hov_wave && over_handle.is_none() { marker_near(input.x) } else { None };
        for (i, &m) in doc.markers.iter().enumerate() {
            let on = hover_marker == Some(i) || self.lab.grab == Some(Grab::Marker(i));
            self.anim.to(wid("lab-mk-h", m, 0), on as u8 as f32, T_HOVER);
        }
        if hov_wave {
            let tip = match over_handle {
                Some(Grab::FadeIn) => "Fade in: drag to set its length",
                Some(Grab::FadeOut) => "Fade out: drag to set its length",
                Some(Grab::CurveIn | Grab::CurveOut) => "Fade curve: drag vertically",
                Some(Grab::TrimStart) => "Start: drag to trim",
                Some(Grab::TrimEnd) => "End: drag to trim",
                _ if hover_marker.is_some() => "Marker: drag to move · right-click: erase",
                _ if doc.chop == Chop::Manual => "Click: add a marker and hear it · hold right-click: erase\nWheel: zoom · Shift+wheel: scroll",
                _ => "Click: hear the slice · double-click: add a marker\nWheel: zoom · Shift+wheel: scroll",
            };
            self.set_tip(tip, "");
        }
        // Molette : zoom autour du curseur ; Maj : défilement.
        if hov_wave && input.wheel != 0.0 {
            let (vs, vl) = self.lab.view;
            if input.mods.shift {
                self.lab.view.0 = (vs - input.wheel as f64 * vl * 0.15).clamp(0.0, (len - vl).max(0.0));
            } else {
                let anchor = vs + ((input.x - r.x) / r.w) as f64 * vl;
                let nl = (vl * 0.8f64.powf(input.wheel as f64)).clamp((r.w as f64 * 0.1).min(len), len);
                let frac = (anchor - vs) / vl;
                self.lab.view = ((anchor - frac * nl).clamp(0.0, (len - nl).max(0.0)), nl);
            }
        }
        // Clic droit maintenu : efface les coupes balayées.
        if hov_wave && input.right_down
            && let Some(i) = marker_near(input.x)
        {
            self.lab_checkpoint("Erase markers");
            self.lab_go_manual();
            if let Some(d) = self.lab.doc.as_mut() {
                d.markers.remove(i);
            }
            self.lab_clamp();
        }
        if hov_wave && input.pressed {
            self.begin_drag(wave_id, input, 0.0);
            if let Some(g) = over_handle {
                let label = match g {
                    Grab::TrimStart | Grab::TrimEnd => "Trim",
                    _ => "Fade",
                };
                self.lab_checkpoint(label);
                self.lab.grab = Some(g);
                self.lab.grab_value = match g {
                    Grab::CurveIn => doc.curve_in,
                    Grab::CurveOut => doc.curve_out,
                    _ => 0.0,
                };
            } else if let Some(i) = hover_marker {
                self.lab_checkpoint("Move marker");
                self.lab_go_manual();
                self.lab.grab = Some(Grab::Marker(i));
            } else {
                let f = f_at(input.x) as usize;
                let k = bounds.partition_point(|&m| m <= f).saturating_sub(1).min(bounds.len() - 2);
                let add = (doc.chop == Chop::Manual || input.double) && f > ta + MIN_GAP && f + MIN_GAP < tb && doc.markers.len() < MAX_SLICES - 1;
                if add {
                    let at = self.lab_zero_crossing(f);
                    self.lab_checkpoint("Add marker");
                    self.lab_go_manual();
                    if let Some(d) = self.lab.doc.as_mut()
                        && !d.markers.iter().any(|&m| m.abs_diff(at) < MIN_GAP)
                    {
                        let i = d.markers.partition_point(|&m| m < at);
                        d.markers.insert(i, at);
                        self.lab.grab = Some(Grab::Marker(i));
                        self.lab_clamp();
                        self.lab_play_slice(k + 1);
                    }
                } else {
                    self.lab_play_slice(k);
                }
            }
        }
        if self.is_dragging(wave_id) {
            let f = f_at(input.x) as usize;
            let y0 = self.drag.as_ref().map_or(input.y, |d| d.y0);
            let curve0 = self.lab.grab_value;
            let grab = self.lab.grab;
            if let Some(d) = self.lab.doc.as_mut() {
                let (ta, tb) = d.trim;
                match grab {
                    Some(Grab::Marker(i)) if i < d.markers.len() => {
                        let lo = if i == 0 { ta } else { d.markers[i - 1] } + MIN_GAP;
                        let hi = d.markers.get(i + 1).copied().unwrap_or(tb).saturating_sub(MIN_GAP);
                        if hi > lo {
                            d.markers[i] = f.clamp(lo, hi);
                        }
                    }
                    Some(Grab::FadeIn) => d.fade_in = f.saturating_sub(ta).min((tb - ta) / 2),
                    Some(Grab::FadeOut) => d.fade_out = tb.saturating_sub(f).min((tb - ta) / 2),
                    Some(Grab::CurveIn) => d.curve_in = (curve0 + (input.y - y0) / 90.0).clamp(-1.0, 1.0),
                    Some(Grab::CurveOut) => d.curve_out = (curve0 + (input.y - y0) / 90.0).clamp(-1.0, 1.0),
                    Some(Grab::TrimStart) => {
                        d.trim.0 = f.min(tb.saturating_sub(MIN_GAP * 4));
                        let n = d.trim.1 - d.trim.0;
                        d.fade_in = d.fade_in.min(n / 2);
                        d.fade_out = d.fade_out.min(n / 2);
                    }
                    Some(Grab::TrimEnd) => {
                        d.trim.1 = f.max(ta + MIN_GAP * 4).min(d.sample.len());
                        let n = d.trim.1 - d.trim.0;
                        d.fade_in = d.fade_in.min(n / 2);
                        d.fade_out = d.fade_out.min(n / 2);
                    }
                    _ => {}
                }
            }
            if input.released && matches!(grab, Some(Grab::Marker(_))) {
                // On entend la tranche que l'on vient d'ajuster.
                let k = self.lab.selected;
                self.lab_clamp();
                self.lab_play_slice(k.min(self.lab.doc.as_ref().map_or(1, Doc::slices) - 1));
            }
            if matches!(grab, Some(Grab::TrimStart | Grab::TrimEnd)) {
                self.lab_clamp();
            }
        }
    }

    /// Passage à zéro le plus proche de `f` (± 2 ms) : une coupe propre.
    fn lab_zero_crossing(&self, f: usize) -> usize {
        let Some(cache) = self.lab.cache.as_ref() else { return f };
        let m = &cache.mono;
        let reach = (self.lab.doc.as_ref().map_or(48_000.0, |d| d.sample.sample_rate) * 0.002) as usize;
        (0..reach)
            .flat_map(|d| [f.saturating_sub(d), f + d])
            .find(|&i| i > 0 && i < m.len() && (m[i - 1] <= 0.0) != (m[i] <= 0.0))
            .unwrap_or(f)
    }

    /// Pads des tranches : clic pour écouter, glisser vers la playlist ou le rack.
    fn lab_pads(&mut self, c: &mut Canvas, input: &Input, r: Rect) {
        let t = self.theme;
        let Some(doc) = self.lab.doc.clone() else { return };
        let bounds = doc.bounds();
        let n = bounds.len() - 1;
        let gap = 6.0;
        let pad_w = ((r.w - gap * (n as f32 - 1.0)) / n as f32).clamp(52.0, 112.0);
        let total = n as f32 * (pad_w + gap) - gap;
        let max_scroll = (total - r.w).max(0.0);
        if input.inside && r.contains(input.x, input.y) && input.wheel != 0.0 {
            self.lab.pad_scroll = (self.lab.pad_scroll - input.wheel * 60.0).clamp(0.0, max_scroll);
        }
        self.lab.pad_scroll = self.lab.pad_scroll.min(max_scroll);
        let scroll = self.anim.to(wid("lab-pad-scroll", 0, 0), self.lab.pad_scroll, T_LAYOUT);
        let playing = self.lab_playhead();
        let selected = self.lab.selected;
        let prev = c.push_clip(Rect::new(r.x - 2.0, r.y - 4.0, r.w + 4.0, r.h + 8.0));
        let Some(cache) = self.lab.cache.as_ref() else { return };
        let mut wave_cols: Vec<(Rect, Color)> = Vec::new();
        let mut rects = Vec::with_capacity(n);
        for k in 0..n {
            let pad = Rect::new(r.x + k as f32 * (pad_w + gap) - scroll, r.y, pad_w, r.h);
            rects.push(pad);
            if pad.right() < r.x || pad.x > r.right() {
                continue;
            }
            let col = pattern_color(k);
            let (s, e) = (bounds[k], bounds[k + 1]);
            let live = playing.is_some_and(|p| p >= s as f64 && p < e as f64);
            let flash = self.anim.get(wid("lab-flash", k, 0)).unwrap_or(0.0);
            let lit = self.anim.to(wid("lab-pad-lit", k, 0), live as u8 as f32, T_FLASH).max(flash);
            let sel = self.anim.to(wid("lab-pad-sel", k, 0), (k == selected) as u8 as f32, T_HOVER);
            c.fill_rrect(pad, 10.0, t.pad_b.mix(col, 0.1 + 0.3 * lit));
            c.fill_rrect(Rect::new(pad.x, pad.y, pad.w, 4.0), 2.0, col.alpha(0.6 + 0.4 * sel));
            if sel > 0.01 {
                c.stroke_rrect(pad, 10.0, 1.5, col.alpha(sel));
            }
            // Mini forme d'onde.
            let g = Rect::new(pad.x + 8.0, pad.y + 28.0, pad.w - 16.0, pad.h - 36.0);
            let span = (e - s).max(1);
            let gw = g.w.max(1.0) as usize;
            for px in 0..gw {
                let fa = s + span * px / gw;
                let fb = s + span * (px + 1) / gw;
                let (ba, bb) = (fa / BUCKET, (fb / BUCKET).max(fa / BUCKET + 1).min(cache.peaks.len()));
                let (lo, hi) = cache.peaks.get(ba..bb).map_or((0.0, 0.0), |w| w.iter().fold((0.0f32, 0.0f32), |(l, h), &(x, y)| (l.min(x), h.max(y))));
                let e = doc.envelope(fa);
                let mid = g.y + g.h * 0.5;
                wave_cols.push((Rect::new(g.x + px as f32, mid - hi * e * g.h * 0.5, 1.0, ((hi - lo) * e * g.h * 0.5).max(1.0)), col.alpha(0.55 + 0.4 * lit)));
            }
        }
        for (rect, col) in wave_cols {
            c.fill_rect(rect, col);
        }
        for (k, pad) in rects.iter().enumerate() {
            if pad.right() < r.x || pad.x > r.right() {
                continue;
            }
            let sel = k == selected;
            self.text(c, self.f.mono_med, 13.0, pad.x + 9.0, pad.y + 16.0, &format!("{}", k + 1), if sel { t.text } else { t.text2 });
            if pad.w >= 66.0 {
                self.text_right(c, self.f.mono, 10.0, pad.right() - 8.0, pad.y + 16.0, &note_name(60 + k as u8), t.muted);
            }
        }
        c.set_clip(Some(prev));

        for (k, pad) in rects.into_iter().enumerate() {
            let id = wid("lab-pad", k, 0);
            let hov = self.hovered(input, id, pad) && pad.right() > r.x && pad.x < r.right();
            if hov {
                let secs = (bounds[k + 1] - bounds[k]) as f32 / doc.sample.sample_rate.max(1.0);
                self.set_tip(&format!("Slice {} · {} · note {}\nClick: play · drag: drop into the playlist or the rack", k + 1, fmt_secs(secs), note_name(60 + k as u8)), "← →");
            }
            if hov && input.pressed {
                self.begin_drag(id, input, 0.0);
                self.lab.grab = Some(Grab::Pad(k, input.x, input.y));
                self.lab_play_slice(k);
            }
            if self.is_dragging(id)
                && let Some(Grab::Pad(k, x0, y0)) = self.lab.grab
                && ((input.x - x0).abs() > 6.0 || (input.y - y0).abs() > 6.0)
            {
                self.lab.grab = None;
                if let Some(path) = self.lab_slice_file(k) {
                    self.begin_dnd(input, Payload::Sample(path));
                    if let Some(d) = self.dnd.as_mut() {
                        d.active = true;
                    }
                }
            }
        }
    }

    /// Colonne de droite : tranche, volume et fondus, tempo et hauteur.
    fn lab_inspector(&mut self, c: &mut Canvas, input: &Input, r: Rect) {
        let t = self.theme;
        let Some(doc) = self.lab.doc.clone() else { return };
        c.fill_rrect(r, 14.0, t.panel);
        c.stroke_rrect(r, 14.0, 1.0, t.hairline);
        let x = r.x + 16.0;
        let w = r.w - 32.0;
        let mut y = r.y + 16.0;
        let section = |app: &mut App, c: &mut Canvas, y: &mut f32, title: &str| {
            if *y > r.y + 20.0 {
                c.fill_rect(Rect::new(x, *y - 8.0, w, 1.0), t.hairline);
            }
            app.text(c, app.f.sans_med, 11.0, x, *y + 6.0, title, t.muted);
            *y += 20.0;
        };
        let sr = doc.sample.sample_rate.max(1.0);
        let bounds = doc.bounds();
        let k = self.lab.selected.min(bounds.len() - 2);

        // Tranche sélectionnée.
        let col = pattern_color(k);
        section(self, c, &mut y, &format!("Slice {}", k + 1));
        c.fill_rrect(Rect::new(x, y, 4.0, 30.0), 2.0, col);
        let secs = (bounds[k + 1] - bounds[k]) as f32 / sr;
        self.text(c, self.f.mono_med, 15.0, x + 14.0, y + 8.0, &fmt_secs(secs), t.text);
        self.text(c, self.f.sans, 11.0, x + 14.0, y + 24.0, &format!("note {} · {} of {}", note_name(60 + k as u8), k + 1, bounds.len() - 1), t.muted);
        y += 40.0;
        let bw = (w - 18.0) / 4.0;
        let row = [
            (Icon::Play, "Play the slice", "Click"),
            (Icon::Swap, "Reverse slice", ""),
            (Icon::ChevronLeft, "Slice one semitone down (slower)", ""),
            (Icon::ChevronRight, "Slice one semitone up (faster)", ""),
        ];
        for (i, (icon, tip, key)) in row.into_iter().enumerate() {
            let b = Rect::new(x + i as f32 * (bw + 6.0), y, bw, 32.0);
            if self.icon_button(c, input, wid("lab-sl", i, 0), b, icon, false, (tip, key)) {
                match i {
                    0 => self.lab_play_slice(k),
                    1 => self.lab_reverse_slice(k),
                    2 => self.lab_pitch_slice(k, -1.0),
                    _ => self.lab_pitch_slice(k, 1.0),
                }
            }
        }
        y += 50.0;

        // Volume et fondus.
        section(self, c, &mut y, "Volume and fades");
        let gain = doc.gain_db;
        let text = format!("{}{} dB", if gain > 0.05 { "+" } else { "" }, fr(gain, 1));
        let norm = (gain + 24.0) / 36.0;
        if let Some(v) = self.lab_slider(c, input, wid("lab-gain", 0, 0), Rect::new(x, y, w - 42.0, 34.0), "Gain", norm, &text, 24.0 / 36.0, "Sample gain") {
            self.lab_checkpoint("Gain");
            if let Some(d) = self.lab.doc.as_mut() {
                d.gain_db = (v * 36.0 - 24.0).clamp(-24.0, 12.0);
            }
        }
        let nb = Rect::new(x + w - 34.0, y + 2.0, 34.0, 32.0);
        if self.icon_button(c, input, wid("lab-norm", 0, 0), nb, Icon::Normalize, false, ("Normalize: peak at 0 dB", "")) {
            self.lab_normalize();
        }
        y += 42.0;
        let half = ((doc.trim.1 - doc.trim.0) / 2).max(1);
        let max_fade = half.min((sr * 2.0) as usize) as f32;
        for (i, (label, frames)) in [("Fade in", doc.fade_in), ("Fade out", doc.fade_out)].into_iter().enumerate() {
            let norm = (frames as f32 / max_fade).sqrt();
            let text = fmt_secs(frames as f32 / sr);
            if let Some(v) = self.lab_slider(c, input, wid("lab-fade", i, 0), Rect::new(x, y, w, 34.0), label, norm, &text, 0.0, label) {
                self.lab_checkpoint("Fade");
                let f = (v * v * max_fade) as usize;
                if let Some(d) = self.lab.doc.as_mut() {
                    if i == 0 { d.fade_in = f } else { d.fade_out = f }
                }
            }
            y += 42.0;
        }
        y += 6.0;

        // Tempo et hauteur.
        section(self, c, &mut y, "Tempo and pitch");
        let field = Rect::new(x, y, w - 42.0, 36.0);
        let bpm = doc.bpm;
        let text = bpm.map_or("—".to_string(), |b| fr(b, 1));
        if let Some(v) = self.drag_field(c, input, wid("lab-bpm", 0, 0), field, bpm.unwrap_or(self.project.bpm), 0.1, 1.0, &text, "orig. BPM") {
            self.lab_checkpoint("Original tempo");
            if let Some(d) = self.lab.doc.as_mut() {
                d.bpm = Some(((v * 10.0).round() / 10.0).clamp(30.0, 300.0));
            }
            if doc.chop == Chop::Grid {
                self.lab_rechop();
            }
        }
        self.tip_zone(input, field, "Original tempo of the sample\nDrag vertically · wheel (Shift: fine)", "");
        let det = Rect::new(x + w - 34.0, y + 2.0, 34.0, 32.0);
        if self.icon_button(c, input, wid("lab-detect", 0, 0), det, Icon::Metronome, false, ("Detect the tempo", "")) {
            self.lab_detect_bpm();
        }
        y += 46.0;
        let pbpm = self.project.bpm;
        self.text(c, self.f.sans, 12.0, x, y + 6.0, &format!("Fit to the project · {} BPM", fr(pbpm, 1)), t.text2);
        y += 18.0;
        let half_w = (w - 8.0) / 2.0;
        let speed = Rect::new(x, y, half_w, 34.0);
        if self.lab_button(c, input, wid("lab-speed", 0, 0), speed, Some(Icon::Reset), "Speed", 0, "Resample: tempo and pitch change together, like a record") {
            self.lab_fit_tempo(false);
        }
        let st = Rect::new(x + half_w + 8.0, y, half_w, 34.0);
        if self.lab_button(c, input, wid("lab-stretch", 0, 0), st, Some(Icon::Stretch), "Stretch", 0, "Stretch (WSOLA): the tempo changes, the pitch stays") {
            self.lab_fit_tempo(true);
        }
        y += 42.0;
        let third = (w - 12.0) / 3.0;
        let items = [(Icon::ChevronLeft, "−1 semitone (length kept)"), (Icon::ChevronRight, "+1 semitone (length kept)"), (Icon::Swap, "Reverse the whole sample")];
        for (i, (icon, tip)) in items.into_iter().enumerate() {
            let b = Rect::new(x + i as f32 * (third + 6.0), y, third, 32.0);
            if self.icon_button(c, input, wid("lab-tr", i, 0), b, icon, false, (tip, "")) {
                match i {
                    0 => self.lab_transpose(-1.0),
                    1 => self.lab_transpose(1.0),
                    _ => self.lab_reverse_all(),
                }
            }
        }
        // Libellés sous les icônes de transposition (règle : un mot avec l'icône).
        let ly = y + 42.0;
        if ly < r.bottom() - 8.0 {
            for (i, l) in ["−1 st", "+1 st", "Reverse"].into_iter().enumerate() {
                let cx = x + i as f32 * (third + 6.0) + third * 0.5;
                self.text_center(c, self.f.sans, 11.0, cx, ly, l, t.muted);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(len: usize) -> Doc {
        let left: Vec<f32> = (0..len).map(|i| ((i as f32) * 0.01).sin()).collect();
        Doc::new(Arc::new(Sample { name: "t".into(), sample_rate: 48_000.0, right: left.clone(), left, markers: Vec::new() }))
    }

    #[test]
    fn render_applies_trim_gain_and_fades() {
        let mut d = doc(10_000);
        d.trim = (1_000, 9_000);
        d.gain_db = -6.0;
        d.fade_in = 1_000;
        d.fade_out = 2_000;
        d.markers = vec![500, 3_000, 6_000, 9_500];
        let s = d.render();
        assert_eq!(s.len(), 8_000);
        assert_eq!(s.markers, [0, 2_000, 5_000]);
        assert_eq!(s.left[0], 0.0, "le fondu part de zéro");
        let g = db_to_gain(-6.0);
        let src = (4_000.0f32 * 0.01).sin();
        assert!((s.left[3_000] - src * g).abs() < 1e-5);
        assert!(s.left[7_999].abs() < 1e-3 * g);
    }

    #[test]
    fn bounds_ignore_markers_outside_the_trim() {
        let mut d = doc(10_000);
        d.trim = (2_000, 8_000);
        d.markers = vec![100, 2_010, 5_000, 7_990, 9_000];
        assert_eq!(d.bounds(), [2_000, 5_000, 8_000]);
        assert_eq!(d.slices(), 2);
    }

    #[test]
    fn fade_shapes() {
        assert_eq!(shape(0.5, 0.0), 0.5);
        assert!(shape(0.5, 1.0) < 0.1);
        assert!(shape(0.5, -1.0) > 0.8);
        assert_eq!(shape(1.0, 0.7), 1.0);
    }

    #[test]
    fn names() {
        assert_eq!(base_name("Break lab"), "Break");
        assert_eq!(base_name("Break lab 3"), "Break");
        assert_eq!(base_name("Break 3"), "Break 3");
        assert_eq!(clean_name("a/b:c"), "a_b_c");
        assert_eq!(fr(1.5, 1), "1.5");
    }

    #[test]
    fn demo_break_has_hits_and_a_tempo() {
        let s = demo_break(48_000.0);
        let a = OnsetAnalysis::stereo(&s.left, &s.right, s.sample_rate);
        let mono = onset::mono(&s.left, &s.right);
        assert!(a.pick(&mono, 0.5, 64).len() >= 12);
        let bpm = tempo::estimate_bpm(&a, s.duration()).expect("tempo");
        assert!((bpm - 92.0).abs() < 0.5, "{bpm}");
    }
}
