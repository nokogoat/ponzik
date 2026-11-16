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
use ponzik_ui::{Canvas, Color, FontId, Path as Shape, Rect};
use ponzik_window::{Key, Modifiers};

use super::Input;
use super::app::{App, Focus, View};
use super::dnd::{DropTarget, Payload};
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
/// Largeur du tiroir « Edit sound ».
const DRAWER_W: f32 = 272.0;
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

/// Les trois étapes de l'Atelier : choisir, découper, utiliser.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Step {
    #[default]
    Sample,
    Cut,
    Use,
}

/// Où mène « Go to it » après une action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Go {
    Nothing,
    Channel(usize),
    Pattern(usize, usize),
    Playlist,
}

/// Confirmation affichée après une action de l'étape « Use ».
struct Done {
    text: String,
    detail: String,
    go: Go,
}

/// Carte d'échantillon proposée à l'étape « Sample » (sans chemin : la boucle de démo).
struct Pick {
    path: Option<String>,
    name: String,
    detail: String,
    bars: Vec<f32>,
}

/// Crêtes d'un échantillon en `n` barres (0 à 1).
fn bars_of(s: &Sample, n: usize) -> Vec<f32> {
    let len = s.len().max(1);
    (0..n)
        .map(|i| {
            let (a, b) = (i * len / n, ((i + 1) * len / n).min(len));
            s.left[a..b].iter().chain(&s.right[a..b]).fold(0.0f32, |m, v| m.max(v.abs())).min(1.0)
        })
        .collect()
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
    step: Step,
    /// Tiroir « Edit sound » ouvert.
    edit_open: bool,
    /// Le tiroir agit sur la tranche choisie plutôt que sur tout le son.
    edit_piece: bool,
    /// Les autres façons de découper sont dépliées.
    more_cut: bool,
    /// Une tranche a déjà été écoutée : la bulle d'aide se retire.
    heard: bool,
    done: Option<Done>,
    /// Cartes de l'étape « Sample », pour ce nombre de fichiers du navigateur.
    picks: Option<(usize, Vec<Pick>)>,
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
            spectral: false,
            preview: None,
            grab: None,
            grab_value: 0.0,
            play_offset: None,
            pad_scroll: 0.0,
            step: Step::Sample,
            edit_open: false,
            edit_piece: false,
            more_cut: false,
            heard: false,
            done: None,
            picks: None,
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

/// Couleur d'une tranche : deux tons de l'accent en alternance, la choisie éclaircie.
fn piece_color(t: &Theme, k: usize, selected: bool) -> Color {
    if selected {
        t.accent_hot
    } else if k % 2 == 0 {
        t.accent
    } else {
        t.accent.mix(t.select, 0.6)
    }
}

/// Cadre en tirets (zone de dépôt).
fn dashed_rect(c: &mut Canvas, r: Rect, col: Color) {
    let (dash, gap, inset) = (8.0f32, 6.0, 14.0);
    let mut x = r.x + inset;
    while x < r.right() - inset {
        let w = dash.min(r.right() - inset - x);
        c.fill_rect(Rect::new(x, r.y, w, 1.5), col);
        c.fill_rect(Rect::new(x, r.bottom() - 1.5, w, 1.5), col);
        x += dash + gap;
    }
    let mut y = r.y + inset;
    while y < r.bottom() - inset {
        let h = dash.min(r.bottom() - inset - y);
        c.fill_rect(Rect::new(r.x, y, 1.5, h), col);
        c.fill_rect(Rect::new(r.right() - 1.5, y, 1.5, h), col);
        y += dash + gap;
    }
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
        lab.step = Step::Cut;
        lab.done = None;
        lab.more_cut = !doc.markers.is_empty();
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
        // PONZIK_LAB_STEP=sample|cut|use, PONZIK_LAB_EDIT (tiroir), PONZIK_LAB_MORE
        // (autres découpes), PONZIK_LAB_DONE (confirmation, sans rien écrire).
        let lab = &mut self.lab;
        match std::env::var("PONZIK_LAB_STEP").as_deref() {
            Ok("sample") => lab.step = Step::Sample,
            Ok("use") => lab.step = Step::Use,
            _ => {}
        }
        lab.edit_open = std::env::var_os("PONZIK_LAB_EDIT").is_some();
        lab.more_cut |= std::env::var_os("PONZIK_LAB_MORE").is_some();
        if std::env::var_os("PONZIK_LAB_DONE").is_some() {
            let n = lab.doc.as_ref().map_or(1, Doc::slices);
            lab.done = Some(Done {
                text: format!("Done! \"{}\" is in your channel rack with a pattern", base_name(&lab.name)),
                detail: format!("{n} pieces on the keys {} to {}. Open the pattern and move the notes around.", note_name(60), note_name(59 + n as u8)),
                go: Go::Pattern(0, self.project.current_pattern),
            });
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
        self.lab.heard = true;
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
        self.lab_checkpoint("Remove a cut");
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
        self.lab_replace_sample("Play backwards", |doc| {
            let s = &doc.sample;
            let len = s.len();
            let rev = |v: &[f32]| v.iter().rev().copied().collect::<Vec<f32>>();
            doc.markers = doc.markers.iter().map(|&m| len - m).rev().collect();
            doc.trim = (len - doc.trim.1, len - doc.trim.0);
            std::mem::swap(&mut doc.fade_in, &mut doc.fade_out);
            std::mem::swap(&mut doc.curve_in, &mut doc.curve_out);
            Sample { left: rev(&s.left), right: rev(&s.right), ..(**s).clone() }
        });
        self.toast("The sound now plays backwards");
    }

    fn lab_reverse_slice(&mut self, k: usize) {
        let Some(b) = self.lab.doc.as_ref().map(Doc::bounds) else { return };
        let (s, e) = (b[k], b[k + 1]);
        self.lab_replace_sample("Play a piece backwards", |doc| {
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
        self.lab_replace_sample("Pitch a piece", |doc| {
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
            self.toast("The loop tempo is unknown: set it in Edit sound first");
            return;
        };
        let to = self.project.bpm;
        if (from - to).abs() < 0.01 {
            self.toast("Already at the project tempo");
            return;
        }
        let factor = from as f64 / to as f64;
        let label = "Match project tempo";
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
        self.lab_replace_sample("Pitch", |doc| {
            let s = &doc.sample;
            let (l, r) = stretch::pitch_shift(&s.left, &s.right, semitones, s.sample_rate);
            Sample { left: l, right: r, ..(**s).clone() }
        });
        self.toast(if semitones > 0.0 { "Pitched up one semitone" } else { "Pitched down one semitone" });
    }

    fn lab_normalize(&mut self) {
        let Some(doc) = self.lab.doc.as_ref() else { return };
        let (a, b) = doc.trim;
        let peak = doc.sample.left[a..b].iter().chain(&doc.sample.right[a..b]).fold(0.0f32, |m, v| m.max(v.abs()));
        if peak < 1e-6 {
            self.toast("Nothing to make louder: the sound is silent");
            return;
        }
        self.lab_checkpoint("Volume");
        if let Some(doc) = self.lab.doc.as_mut() {
            doc.gain_db = (-20.0 * peak.log10()).clamp(-24.0, 24.0);
        }
        self.toast("As loud as possible without distorting");
    }

    fn lab_detect_bpm(&mut self) {
        let Some(cache) = self.lab.cache.as_ref() else { return };
        let Some(doc) = self.lab.doc.as_ref() else { return };
        let sr = doc.sample.sample_rate.max(1.0);
        let dur = (doc.trim.1 - doc.trim.0) as f32 / sr;
        let bpm = tempo::estimate_bpm(&cache.onsets, doc.sample.duration()).map(|b| tempo::snap_to_loop(b, dur));
        match bpm {
            Some(b) => {
                self.lab_checkpoint("Loop tempo");
                if let Some(doc) = self.lab.doc.as_mut() {
                    doc.bpm = Some(b);
                }
                self.toast(&format!("Detected tempo: {} BPM", fr(b, 1)));
                if self.lab.doc.as_ref().is_some_and(|d| d.chop == Chop::Grid) {
                    self.lab_rechop();
                }
            }
            None => self.toast("No clear beat found: type the tempo in instead"),
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
            self.lab_done(format!("Saved as one file: {short}"), "It is in the Échantillons folder, with the cuts inside. Find it in the browser's sample list.".into(), Go::Nothing);
        }
    }

    /// Chaque tranche dans son propre fichier.
    fn lab_save_pieces(&mut self) {
        let n = self.lab.doc.as_ref().map_or(0, Doc::slices);
        let saved = (0..n).filter(|&k| self.lab_slice_file(k).is_some()).count();
        if saved > 0 {
            let name = base_name(&self.lab.name);
            let detail = format!("They are in the Échantillons folder as \"{name} slice 1\" and so on. Find them in the browser's sample list.");
            self.lab_done(format!("Saved {saved} pieces as separate files"), detail, Go::Nothing);
        }
    }

    fn lab_done(&mut self, text: String, detail: String, go: Go) {
        self.lab.done = Some(Done { text, detail, go });
        self.anim.set(wid("lab-done", 0, 0), 0.0);
    }

    /// « Go to it » : ouvre ce que l'action vient de créer.
    fn lab_go(&mut self, go: Go) {
        match go {
            Go::Nothing => {}
            Go::Channel(ch) => {
                self.focus = Focus::Channel(ch.min(self.project.channels.len().saturating_sub(1)));
                self.set_view(View::Rack);
            }
            Go::Pattern(ch, p) => {
                if self.project.current_pattern != p && p < self.project.patterns.len() {
                    self.select_pattern(p);
                }
                self.focus = Focus::Channel(ch.min(self.project.channels.len().saturating_sub(1)));
                self.set_view(View::Piano);
            }
            Go::Playlist => self.set_view(View::Playlist),
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
                self.lab_done(format!("\"{name}\" now plays your edited version"), "Ctrl+Z brings the old sound back.".into(), Go::Channel(ch));
            }
            Source::Clip(i) if i < self.project.audio_clips.len() => {
                let secs = self.samples.get(&path).map_or(0.0, |s| s.duration() as f64);
                let ticks = (secs * self.project.bpm as f64 / 60.0 * PPQ as f64).round().max(24.0) as u32;
                let clip = &mut self.project.audio_clips[i];
                clip.path = path;
                clip.offset = 0.0;
                clip.len = ticks;
                self.commit("Sample Lab: replace clip");
                self.lab_done("The playlist clip now plays your edited version".into(), "Ctrl+Z brings the old sound back.".into(), Go::Playlist);
            }
            _ => self.lab_save_action(),
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
        let keys = format!("{n} piece{} on the keys {} to {}", if n > 1 { "s" } else { "" }, note_name(60), note_name(59 + n as u8));
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
            let detail = format!("{keys}. Open the pattern and move the notes around to make your own beat.");
            self.lab_done(format!("Done! Pattern \"Flip {name}\" rebuilds the loop"), detail, Go::Pattern(ch, p));
        } else {
            self.commit("Slices to the rack");
            let detail = format!("{keys}. Play them with your computer keyboard or draw notes in the piano roll.");
            self.lab_done(format!("Done! \"{name} slices\" is in your channel rack"), detail, Go::Channel(ch));
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
        if !input.down {
            self.lab.grab = None;
        }
        // Un échantillon glissé depuis le navigateur s'ouvre ici.
        self.drop_zone(input, body, DropTarget::Lab);
        self.lab_steps(c, input, header);
        let step = if self.lab.doc.is_some() { self.lab.step } else { Step::Sample };
        if step == Step::Sample {
            self.lab_sample_step(c, input, body);
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

        // Tiroir « Edit sound » : glisse depuis la droite.
        let open = self.anim.to(wid("lab-drawer", 0, 0), self.lab.edit_open as u8 as f32, T_LAYOUT);
        let dw = (DRAWER_W + 14.0) * open;
        let main = Rect::new(body.x, body.y, body.w - dw, body.h);
        match step {
            Step::Cut => self.lab_cut_step(c, input, main),
            _ => self.lab_use_step(c, input, main),
        }
        if open > 0.01 {
            let d = Rect::new(main.right() + 14.0, body.y, DRAWER_W, body.h);
            let prev = c.push_clip(Rect::new(main.right(), body.y, dw + 1.0, body.h));
            self.lab_drawer(c, input, d);
            c.set_clip(Some(prev));
        }
    }

    /// Les trois étapes, toujours visibles ; l'étape en cours est allumée.
    fn lab_steps(&mut self, c: &mut Canvas, input: &Input, r: Rect) {
        let t = self.theme;
        let has = self.lab.doc.is_some();
        let cur = if has { self.lab.step } else { Step::Sample };
        let name = self.lab.name.clone();
        let steps = [
            (Step::Sample, "Sample", "Step 1: choose a sound\nDrop a file, pick a recent one or try the example loop"),
            (Step::Cut, "Cut", "Step 2: cut the sound into pieces\nClick a piece to hear it"),
            (Step::Use, "Use", "Step 3: use the pieces\nPlay them on pads or rebuild the loop as a pattern"),
        ];
        let ci = steps.iter().position(|s| s.0 == cur).unwrap_or(0);
        let cy = r.y + r.h * 0.5;
        let mut x = r.x;
        for (i, (step, label, tip)) in steps.into_iter().enumerate() {
            let extra = if i == 0 && has { self.fit(self.f.sans, 13.0, &name, 150.0) } else { String::new() };
            let lw = self.fonts.measure(self.f.sans_med, 14.0, label);
            let ew = if extra.is_empty() { 0.0 } else { self.fonts.measure(self.f.sans, 13.0, &extra) + 8.0 };
            let pill = Rect::new(x, r.y, 8.0 + 24.0 + 10.0 + lw + ew + 16.0, r.h);
            let enabled = i == 0 || has;
            let id = wid("lab-step", i, 0);
            let hov = enabled && self.hovered(input, id, pill);
            let hv = self.hover_anim(id, hov);
            let on = self.anim.to(id ^ 0x4f4e, (i == ci) as u8 as f32, T_LAYOUT);
            c.fill_rrect(pill, r.h * 0.5, t.raised.alpha(hv * (1.0 - on)));
            if on > 0.01 {
                c.fill_rrect(pill, r.h * 0.5, t.accent.alpha(0.13 * on));
                c.stroke_rrect(pill, r.h * 0.5, 1.0, t.accent.alpha(0.5 * on));
            }
            let ccx = pill.x + 8.0 + 12.0;
            let number = format!("{}", i + 1);
            if i == ci {
                c.fill_circle(ccx, cy, 12.0, t.accent);
                self.text_center(c, self.f.sans_med, 13.0, ccx, cy, &number, t.on_accent);
            } else if has && i < ci {
                c.fill_circle(ccx, cy, 12.0, t.accent.alpha(0.18));
                draw_icon(c, Icon::Check, ccx, cy, 13.0, t.accent);
            } else {
                let col = if enabled { t.text2 } else { t.muted.alpha(0.6) };
                c.stroke_circle(ccx, cy, 11.0, 1.5, col);
                self.text_center(c, self.f.sans_med, 13.0, ccx, cy, &number, col);
            }
            let col = if i == ci { t.text } else if enabled { t.text2.mix(t.text, hv) } else { t.muted.alpha(0.6) };
            self.text(c, self.f.sans_med, 14.0, ccx + 22.0, cy, label, col);
            if !extra.is_empty() {
                self.text(c, self.f.sans, 13.0, ccx + 22.0 + lw + 8.0, cy, &extra, t.muted);
            }
            if hov {
                self.set_tip(tip, "");
                self.set_cursor(ponzik_window::Cursor::Hand);
                if input.pressed {
                    self.lab.step = step;
                }
            }
            x = pill.right();
            if i < 2 {
                draw_icon(c, Icon::ChevronRight, x + 14.0, cy, 14.0, t.muted.alpha(0.7));
                x += 28.0;
            }
        }
        // À droite : le tiroir des retouches.
        if has && cur != Step::Sample {
            let label = "Edit sound";
            let w = self.button_width(true, label);
            let b = Rect::new(r.right() - w, r.y, w, r.h);
            let open = self.lab.edit_open;
            let tip = "Edit sound\nVolume, fades, tempo, pitch and playing backwards";
            if self.lab_button(c, input, wid("lab-edit", 0, 0), b, Some(Icon::Mixer), label, if open { 2 } else { 0 }, tip) {
                self.lab.edit_open = !open;
            }
        }
    }

    /// Cartes de l'étape « Sample » : la boucle de démo et les fichiers récents.
    fn lab_refresh_picks(&mut self) {
        let n = self.sample_files.len();
        if self.lab.picks.as_ref().is_some_and(|(k, _)| *k == n) {
            return;
        }
        let demo = demo_break(self.sample_rate as f32);
        let mut picks = vec![Pick {
            path: None,
            name: "Example drum loop".into(),
            detail: format!("Drum break · {}", fmt_secs(demo.duration())),
            bars: bars_of(&demo, 40),
        }];
        let mut files: Vec<(std::time::SystemTime, PathBuf)> = self
            .sample_files
            .iter()
            .filter_map(|p| {
                let m = std::fs::metadata(p).ok()?;
                (m.len() < 32 << 20).then_some((m.modified().ok()?, p.clone()))
            })
            .collect();
        files.sort_by_key(|f| std::cmp::Reverse(f.0));
        for (_, p) in files.into_iter().take(12) {
            if picks.len() >= 6 {
                break;
            }
            let key = p.to_string_lossy().into_owned();
            let sample = match self.samples.get(&key) {
                Some(s) => s.clone(),
                None => match super::compat::load_sample(&key) {
                    Ok(s) => {
                        let s = Arc::new(s);
                        self.samples.insert(key.clone(), s.clone());
                        s
                    }
                    Err(_) => continue,
                },
            };
            // Trop court pour être découpé.
            if sample.duration() < 0.3 {
                continue;
            }
            let name = p.file_stem().map_or(String::new(), |s| s.to_string_lossy().into_owned());
            picks.push(Pick { path: Some(key), name, detail: format!("Recent · {}", fmt_secs(sample.duration())), bars: bars_of(&sample, 40) });
        }
        self.lab.picks = Some((n, picks));
    }

    /// Étape 1 : déposer, choisir ou essayer un son.
    fn lab_sample_step(&mut self, c: &mut Canvas, input: &Input, r: Rect) {
        let t = self.theme;
        self.lab_refresh_picks();
        c.fill_rrect(r, 16.0, t.panel);
        let dragging = self.dnd.as_ref().is_some_and(|d| d.active);
        let glow = self.anim.to(wid("lab-drop", 0, 0), dragging as u8 as f32, T_HOVER);
        dashed_rect(c, r.inset(12.0), t.text.alpha(0.14).mix(t.accent, glow));
        let current = self.lab.doc.as_ref().map(|_| self.lab.name.clone());
        let picks: Vec<(Option<String>, String, String, Vec<f32>)> = self
            .lab
            .picks
            .as_ref()
            .map(|(_, p)| p.iter().map(|p| (p.path.clone(), p.name.clone(), p.detail.clone(), p.bars.clone())).collect())
            .unwrap_or_default();
        let card_h = 124.0;
        let content_h = 64.0 + 24.0 + 30.0 + 28.0 + if current.is_some() { 56.0 } else { 0.0 } + 44.0 + card_h + 40.0;
        let cx = r.center().0;
        let mut y = r.y + ((r.h - content_h) * 0.5).max(28.0);

        // Illustration : une boucle coupée en morceaux.
        let w = 300.0f32.min(r.w - 80.0);
        for i in 0..60 {
            let x = cx - w * 0.5 + i as f32 * w / 60.0;
            let k = i / 10;
            let h = 6.0 + 50.0 * ((i as f32 * 0.7).sin() * 0.5 + 0.5) * (-((i % 10) as f32) / 5.0).exp();
            c.fill_rrect(Rect::new(x, y + 32.0 - h * 0.5, 2.5, h), 1.0, piece_color(&t, k, false).alpha(0.8));
        }
        for k in 1..6 {
            let x = cx - w * 0.5 + k as f32 * w / 6.0 - 2.0;
            c.fill_rect(Rect::new(x, y, 1.5, 64.0), t.text.alpha(0.45));
        }
        y += 64.0 + 24.0;
        let title = if dragging { "Drop it here" } else { "Drop a sample here" };
        self.text_center(c, self.f.sans_med, 22.0, cx, y + 12.0, title, t.text);
        y += 30.0;
        let line = "The Sample Lab cuts a loop into pieces so you can play them on pads.";
        self.text_center(c, self.f.sans, 14.0, cx, y + 10.0, line, t.text2);
        y += 28.0;
        if let Some(name) = current {
            let label = format!("Keep working on {}", self.fit(self.f.sans_med, 13.0, &name, 220.0));
            let bw = self.button_width(true, &label) + 6.0;
            let b = Rect::new(cx - bw * 0.5, y + 10.0, bw, 38.0);
            if self.lab_button(c, input, wid("lab-resume", 0, 0), b, Some(Icon::ChevronRight), &label, 3, "Back to cutting the sound that is already open") {
                self.lab.step = Step::Cut;
            }
            y += 56.0;
        }
        y += 18.0;
        self.text_center(c, self.f.sans_med, 13.0, cx, y, "or pick one", t.muted);
        y += 26.0;

        // Cartes.
        let n = picks.len().max(1);
        let gap = 14.0;
        let card_w = ((r.w - 80.0 - gap * (n as f32 - 1.0)) / n as f32).min(210.0);
        let row_w = n as f32 * (card_w + gap) - gap;
        for (i, (path, name, detail, bars)) in picks.into_iter().enumerate() {
            let card = Rect::new(cx - row_w * 0.5 + i as f32 * (card_w + gap), y, card_w, card_h);
            let id = wid("lab-pick", i, 0);
            let hov = self.hovered(input, id, card);
            let hv = self.hover_anim(id, hov);
            let press = self.anim.to(id ^ 0x5052, if hov && input.down { 0.97 } else { 1.0 }, T_PRESS);
            let cr = card.scale(press).translate(0.0, -2.0 * hv);
            c.fill_rrect(cr, 12.0, t.raised.mix(t.raised_hi, hv * 0.6));
            c.stroke_rrect(cr, 12.0, 1.0, t.hairline.mix(t.accent.alpha(0.6), hv));
            let g = Rect::new(cr.x + 14.0, cr.y + 14.0, cr.w - 28.0, 54.0);
            c.fill_rrect(g.inset(-4.0), 8.0, t.well);
            let bw = g.w / bars.len().max(1) as f32;
            for (j, &b) in bars.iter().enumerate() {
                let h = (b * g.h).max(2.0);
                let col = piece_color(&t, j * 6 / bars.len().max(1), false).alpha(0.6 + 0.35 * hv);
                c.fill_rrect(Rect::new(g.x + j as f32 * bw + 0.5, g.y + (g.h - h) * 0.5, (bw - 1.5).max(1.0), h), 1.0, col);
            }
            if path.is_none() {
                let tag = "Try it";
                let tw = self.fonts.measure(self.f.sans_med, 11.0, tag) + 14.0;
                let tr = Rect::new(cr.right() - 14.0 - tw, cr.y + 100.0, tw, 20.0);
                c.fill_rrect(tr, 10.0, t.accent.alpha(0.18 + 0.2 * hv));
                self.text_center(c, self.f.sans_med, 11.0, tr.x + tw * 0.5, tr.y + 10.0, tag, t.accent);
            }
            self.text_fit(c, self.f.sans_med, 13.0, cr.x + 14.0, cr.y + 92.0, &name, cr.w - 28.0, t.text);
            self.text_fit(c, self.f.sans, 12.0, cr.x + 14.0, cr.y + 110.0, &detail, cr.w - 28.0, t.muted);
            if hov {
                let tip = match path {
                    Some(_) => format!("Open \"{name}\"\nYou will cut it into pieces in the next step"),
                    None => "Open an example drum loop\nA good way to see how the Sample Lab works".to_string(),
                };
                self.set_tip(&tip, "");
                self.set_cursor(ponzik_window::Cursor::Hand);
                if input.pressed {
                    match path {
                        Some(p) => self.open_in_lab(&p, Source::Browser),
                        None => {
                            let sample = Arc::new(demo_break(self.sample_rate as f32));
                            self.open_sample_in_lab(sample, None, Source::Browser);
                        }
                    }
                    return;
                }
            }
        }
        y += card_h + 26.0;
        let foot = "You can also drag a sample here from the browser, or double-click an audio clip in the playlist.";
        self.text_center(c, self.f.sans, 12.0, cx, y, foot, t.muted);
    }

    /// Bouton pilule : icône et libellé. `style` : 0 normal, 1 principal,
    /// 2 enfoncé, 3 principal avec l'icône à droite (« suivant »).
    #[allow(clippy::too_many_arguments)]
    fn lab_button(&mut self, c: &mut Canvas, input: &Input, id: u64, r: Rect, icon: Option<Icon>, label: &str, style: u8, tip: &str) -> bool {
        let t = self.theme;
        let hov = self.hovered(input, id, r);
        let hv = self.hover_anim(id, hov);
        let press = self.anim.to(id ^ 0x5052, if hov && input.down { 0.96 } else { 1.0 }, T_PRESS);
        let rr = r.scale(press);
        let primary = style == 1 || style == 3;
        let (fill, fg) = match style {
            1 | 3 => (t.accent.mix(t.accent_hot, hv * 0.2), t.on_accent),
            2 => (t.raised_hi.mix(t.accent, 0.18), t.accent),
            _ => (t.raised.mix(t.raised_hi, hv), t.text2.mix(t.text, hv)),
        };
        c.fill_rrect(rr, 10.0, fill);
        if !primary {
            c.stroke_rrect(rr, 10.0, 1.0, if style == 2 { t.accent.alpha(0.5) } else { t.hairline });
        }
        let cy = rr.y + rr.h * 0.5;
        match icon {
            Some(icon) if style == 3 => {
                let max = rr.w - 50.0;
                self.text_fit(c, self.f.sans_med, 13.0, rr.x + 16.0, cy, label, max, fg);
                draw_icon(c, icon, rr.right() - 20.0 + 2.0 * hv, cy, 15.0 * press, fg);
            }
            Some(icon) => {
                draw_icon(c, icon, rr.x + 21.0, cy, 15.0 * press, fg);
                self.text_fit(c, self.f.sans_med, 13.0, rr.x + 36.0, cy, label, rr.right() - 10.0 - rr.x - 36.0, fg);
            }
            None => {
                let s = self.fit(self.f.sans_med, 13.0, label, rr.w - 12.0);
                self.text_center(c, self.f.sans_med, 13.0, rr.x + rr.w * 0.5, cy, &s, fg);
            }
        }
        if hov {
            if !tip.is_empty() {
                self.set_tip(tip, "");
            }
            self.set_cursor(ponzik_window::Cursor::Hand);
        }
        hov && input.pressed
    }

    fn button_width(&self, icon: bool, label: &str) -> f32 {
        self.fonts.measure(self.f.sans_med, 13.0, label) + if icon { 50.0 } else { 28.0 }
    }

    /// Lignes de `s` coupées aux mots pour tenir dans `max_w`.
    fn wrap(&self, f: FontId, size: f32, s: &str, max_w: f32) -> Vec<String> {
        let mut lines = Vec::new();
        let mut cur = String::new();
        for word in s.split_whitespace() {
            let cand = if cur.is_empty() { word.to_string() } else { format!("{cur} {word}") };
            if !cur.is_empty() && self.fonts.measure(f, size, &cand) > max_w {
                lines.push(std::mem::replace(&mut cur, word.to_string()));
            } else {
                cur = cand;
            }
        }
        if !cur.is_empty() {
            lines.push(cur);
        }
        lines
    }

    /// Curseur horizontal : libellé, valeur, piste. Renvoie la nouvelle position (0 à 1).
    #[allow(clippy::too_many_arguments)]
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
            self.set_tip(&format!("{tip}\nDrag or scroll to change · double-click to reset"), "");
        }
        out
    }

    /// « Fewer pieces ↔ More pieces » : la sensibilité des transitoires, en clair.
    fn lab_amount(&mut self, c: &mut Canvas, input: &Input, r: Rect, norm: f32) -> Option<f32> {
        let t = self.theme;
        let id = wid("lab-sens", 0, 0);
        let (left, right) = ("Fewer pieces", "More pieces");
        let lw = self.fonts.measure(self.f.sans, 13.0, left);
        let rw = self.fonts.measure(self.f.sans, 13.0, right);
        let cy = r.y + r.h * 0.5;
        let track = Rect::new(r.x + lw + 16.0, cy - 3.0, (r.w - lw - rw - 32.0).max(40.0), 6.0);
        let hit = Rect::new(track.x - 12.0, r.y - 4.0, track.w + 24.0, r.h + 8.0);
        let hov = self.hovered(input, id, hit);
        let mut out = None;
        if hov && input.pressed {
            if input.double {
                out = Some(0.5);
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
        self.text(c, self.f.sans, 13.0, r.x, cy, left, t.text2.mix(t.text, (1.0 - shown) * 0.6));
        self.text(c, self.f.sans, 13.0, track.right() + 16.0, cy, right, t.text2.mix(t.text, shown * 0.6));
        c.fill_rrect(track, 3.0, t.raised_hi);
        c.fill_rrect(Rect::new(track.x, track.y, (track.w * shown).max(6.0), track.h), 3.0, t.accent.mix(t.accent_hot, hv * 0.2));
        let kx = track.x + track.w * shown;
        c.fill_circle(kx, cy, 9.0 + hv, t.text);
        c.fill_circle(kx, cy, 3.5, t.accent_deep);
        if hov {
            self.set_cursor(ponzik_window::Cursor::Hand);
            self.set_tip("How finely to cut\nLeft: only the big hits · right: every little sound\nDrag or scroll · double-click to reset", "");
        }
        out
    }

    /// Bouton texte qui déplie ou replie un groupe d'options.
    fn lab_disclosure(&mut self, c: &mut Canvas, input: &Input, r: Rect, label: &str, open: bool, tip: &str) -> bool {
        let t = self.theme;
        let id = wid("lab-more", 0, 0);
        let hov = self.hovered(input, id, r);
        let hv = self.hover_anim(id, hov);
        c.fill_rrect(r, 10.0, t.raised.alpha(hv));
        let col = t.text2.mix(t.text, hv);
        let cy = r.y + r.h * 0.5;
        self.text(c, self.f.sans_med, 13.0, r.x + 12.0, cy, label, col);
        let (x, d) = (r.right() - 16.0, if open { -1.0 } else { 1.0 });
        c.stroke_polyline(&[(x - 4.5, cy - 2.0 * d), (x, cy + 2.5 * d), (x + 4.5, cy - 2.0 * d)], 1.6, col);
        if hov {
            self.set_tip(tip, "");
            self.set_cursor(ponzik_window::Cursor::Hand);
        }
        hov && input.pressed
    }

    // --- Étape « Cut » ---

    fn lab_cut_step(&mut self, c: &mut Canvas, input: &Input, main: Rect) {
        let t = self.theme;
        let bar = Rect::new(main.x, main.y, main.w, 76.0);
        self.lab_cut_bar(c, input, bar);
        let mut y = bar.bottom() + 10.0;
        if self.lab.more_cut {
            let row = Rect::new(main.x, y, main.w, 60.0);
            self.lab_cut_methods(c, input, row);
            y = row.bottom() + 10.0;
        }
        let len = self.lab.doc.as_ref().map_or(1.0, |d| d.sample.len().max(1) as f64);
        let zoomed = self.lab.view.1 < len - 1.0;
        if zoomed {
            let ov = Rect::new(main.x, y, main.w, 28.0);
            self.lab_overview(c, input, ov);
            y = ov.bottom() + 8.0;
        }
        let pads_h = 72.0;
        let tools_w = if zoomed { 72.0 } else { 38.0 };
        let ruler = Rect::new(main.x, y, main.w - tools_w, 26.0);
        let wave = Rect::new(main.x, ruler.bottom(), main.w, (main.bottom() - pads_h - 10.0 - ruler.bottom()).max(60.0));
        let pads = Rect::new(main.x, wave.bottom() + 10.0, main.w, pads_h);
        c.fill_rrect(Rect::new(main.x, ruler.y, main.w, ruler.h + wave.h), 12.0, t.well);
        self.lab_ruler(c, input, ruler);
        // Petits outils de vue, au bout de la règle.
        let b = Rect::new(main.right() - 34.0, ruler.y + 1.0, 30.0, 24.0);
        let on = self.lab.spectral;
        let tip = if on { "Color by tone is on\nDeep sounds in teal, bright sounds in pale green · click to turn off" } else { "Color by tone\nShows deep sounds in teal and bright sounds in pale green" };
        if self.icon_button(c, input, wid("lab-spec", 0, 0), b, Icon::Spectral, on, (tip, "")) {
            self.lab.spectral = !on;
        }
        if zoomed && self.icon_button(c, input, wid("lab-all", 0, 0), b.translate(-34.0, 0.0), Icon::Columns, false, ("Zoom out to see the whole sound", "Home")) {
            self.lab_zoom_all();
        }
        self.lab_wave(c, input, wave);
        self.lab_pads(c, input, pads);
        self.lab_hint(c, wave);
    }

    /// Bandeau de l'étape « Cut » : écouter, découper, passer à la suite.
    fn lab_cut_bar(&mut self, c: &mut Canvas, input: &Input, r: Rect) {
        let t = self.theme;
        let Some(doc) = self.lab.doc.as_ref() else { return };
        let (chop, sens, division, bpm, n) = (doc.chop, doc.sensitivity, doc.division, doc.bpm, doc.slices());
        c.fill_rrect(r, 14.0, t.panel);
        c.stroke_rrect(r, 14.0, 1.0, t.hairline);
        let cy = r.y + r.h * 0.5;

        // Écouter.
        let pr = 24.0;
        let px = r.x + 16.0 + pr;
        let id = wid("lab-play", 0, 0);
        let hit = Rect::new(r.x + 8.0, r.y + 6.0, 162.0, r.h - 12.0);
        let hov = self.hovered(input, id, hit);
        let hv = self.hover_anim(id, hov);
        let press = self.anim.to(id ^ 0x5052, if hov && input.down { 0.93 } else { 1.0 }, T_PRESS);
        let playing = self.lab_playhead().is_some();
        c.fill_circle(px, cy, pr * press, t.accent.mix(t.accent_hot, hv * 0.25));
        let icon = if playing { Icon::Stop } else { Icon::Play };
        draw_icon(c, icon, px + if playing { 0.0 } else { 1.5 }, cy, 20.0 * press, t.on_accent);
        self.text(c, self.f.sans_med, 14.0, px + pr + 12.0, cy - 9.0, if playing { "Stop" } else { "Listen" }, t.text);
        self.text(c, self.f.sans, 12.0, px + pr + 12.0, cy + 10.0, "or press Space", t.muted);
        if hov {
            self.set_tip("Listen from the selected piece to the end\nClick a piece on the wave to hear only that piece", "Space");
            self.set_cursor(ponzik_window::Cursor::Hand);
            if input.pressed {
                self.lab_toggle_play();
            }
        }
        let div_x = r.x + 186.0;
        c.fill_rect(Rect::new(div_x, r.y + 14.0, 1.0, r.h - 28.0), t.hairline);

        // À droite : la suite, et les autres façons de découper.
        let next = "Next: use the pieces";
        let w = self.button_width(true, next) + 6.0;
        let nb = Rect::new(r.right() - 16.0 - w, cy - 20.0, w, 40.0);
        if self.lab_button(c, input, wid("lab-next", 0, 0), nb, Some(Icon::ChevronRight), next, 3, "Go to step 3\nPlay the pieces on pads or turn them into a beat") {
            self.lab.step = Step::Use;
        }
        let open = self.lab.more_cut;
        let more = if open { "Fewer options" } else { "More ways to cut" };
        let mw = self.fonts.measure(self.f.sans_med, 13.0, more) + 42.0;
        let mb = Rect::new(nb.x - 12.0 - mw, cy - 18.0, mw, 36.0);
        let tip = if open { "Hide the other ways to cut" } else { "Other ways to cut\nOn the beat, or by hand" };
        if self.lab_disclosure(c, input, mb, more, open, tip) {
            self.lab.more_cut = !open;
        }

        // Au centre : la découpe en cours, en clair.
        let x0 = div_x + 22.0;
        let x1 = mb.x - 24.0;
        let title = match chop {
            Chop::Transients => "Cut automatically",
            Chop::Grid => "Cut on the beat",
            Chop::Manual => "Cut by hand",
        };
        self.text(c, self.f.sans_med, 14.0, x0, cy - 15.0, title, t.text);
        let count = format!("{n} piece{}", if n == 1 { "" } else { "s" });
        let cx = self.text_right(c, self.f.sans_med, 14.0, x1, cy - 15.0, &count, t.accent);
        self.tip_zone(input, Rect::new(cx, cy - 26.0, x1 - cx, 22.0), "How many pieces the sound is cut into\nEach piece will play on its own key or pad", "");
        let row = Rect::new(x0, cy + 1.0, x1 - x0, 26.0);
        match chop {
            Chop::Transients => {
                if let Some(v) = self.lab_amount(c, input, row, sens) {
                    self.lab_checkpoint("Number of pieces");
                    if let Some(doc) = self.lab.doc.as_mut() {
                        doc.sensitivity = v;
                    }
                    self.lab_rechop();
                }
            }
            Chop::Grid => {
                let roomy = row.w >= 520.0;
                let mut x = row.x;
                let rcy = row.y + row.h * 0.5;
                if roomy {
                    self.text(c, self.f.sans, 13.0, x, rcy, "Piece length", t.text2);
                    x += self.fonts.measure(self.f.sans, 13.0, "Piece length") + 10.0;
                }
                let seg = Rect::new(x, row.y - 2.0, 196.0, 30.0);
                let labels: Vec<&str> = DIVISIONS.iter().map(|d| d.1).collect();
                let tips = [
                    ("Quarter notes\n4 pieces per bar, one per beat", ""),
                    ("Eighth notes\n8 pieces per bar", ""),
                    ("Sixteenth notes\n16 pieces per bar", ""),
                    ("Thirty-second notes\n32 pieces per bar", ""),
                ];
                if let Some(i) = self.segmented_tips(c, input, wid("lab-div", 0, 0), seg, &labels, &tips, division) {
                    self.lab_checkpoint("Piece length");
                    if let Some(doc) = self.lab.doc.as_mut() {
                        doc.division = i;
                    }
                    self.lab_rechop();
                }
                let mut x = seg.right() + 16.0;
                if roomy {
                    self.text(c, self.f.sans, 13.0, x, rcy, "Loop tempo", t.text2);
                    x += self.fonts.measure(self.f.sans, 13.0, "Loop tempo") + 10.0;
                }
                let field = Rect::new(x, row.y - 2.0, 112.0, 30.0);
                let v = bpm.unwrap_or(self.project.bpm);
                if let Some(nv) = self.drag_field(c, input, wid("lab-grid-bpm", 0, 0), field, v, 0.1, 1.0, &fr(v, 1), "BPM") {
                    let nv = (nv * 10.0).round() / 10.0;
                    self.lab_checkpoint("Loop tempo");
                    if let Some(doc) = self.lab.doc.as_mut() {
                        doc.bpm = Some(nv.clamp(30.0, 300.0));
                    }
                    self.lab_rechop();
                }
                self.tip_zone(input, field, "Tempo of the loop, in beats per minute\nThe cuts follow it · drag up or down, or scroll", "");
            }
            Chop::Manual => {
                let rcy = row.y + row.h * 0.5;
                draw_icon(c, Icon::Info, row.x + 7.0, rcy, 14.0, t.muted);
                let line = "Click the wave to add a cut · drag a cut to move it · hold right-click to remove it";
                self.text_fit(c, self.f.sans, 13.0, row.x + 22.0, rcy, line, row.w - 22.0, t.text2);
            }
        }
    }

    /// Les trois façons de découper, en cartes.
    fn lab_cut_methods(&mut self, c: &mut Canvas, input: &Input, r: Rect) {
        let t = self.theme;
        let Some(chop) = self.lab.doc.as_ref().map(|d| d.chop) else { return };
        let items = [
            (Chop::Transients, Icon::Slice, "Automatically", "Finds each hit in the sound", "Cut automatically\nA new piece starts at each drum hit or note"),
            (Chop::Grid, Icon::Metronome, "On the beat", "Equal pieces that follow the tempo", "Cut on the beat\nEqual pieces, like the steps of a step sequencer"),
            (Chop::Manual, Icon::Pencil, "By hand", "You click where to cut", "Cut by hand\nClick the wave to add cuts, drag them, hold right-click to remove them"),
        ];
        let gap = 10.0;
        let w = (r.w - 2.0 * gap) / 3.0;
        for (i, (mode, icon, title, desc, tip)) in items.into_iter().enumerate() {
            let card = Rect::new(r.x + i as f32 * (w + gap), r.y, w, r.h);
            let id = wid("lab-method", i, 0);
            let on = chop == mode;
            let hov = self.hovered(input, id, card);
            let hv = self.hover_anim(id, hov);
            let onv = self.anim.to(id ^ 0x4f4e, on as u8 as f32, T_HOVER);
            c.fill_rrect(card, 12.0, t.panel.mix(t.raised, hv * (1.0 - onv)));
            if onv > 0.01 {
                c.fill_rrect(card, 12.0, t.accent.alpha(0.1 * onv));
            }
            c.stroke_rrect(card, 12.0, 1.0, t.hairline.mix(t.accent.alpha(0.7), onv));
            let (ix, cy) = (card.x + 32.0, card.y + card.h * 0.5);
            c.fill_circle(ix, cy, 17.0, t.raised_hi.mix(t.accent, onv));
            draw_icon(c, icon, ix, cy, 16.0, t.text2.mix(t.on_accent, onv));
            let max = card.right() - 12.0 - (ix + 28.0);
            self.text_fit(c, self.f.sans_med, 13.0, ix + 28.0, cy - 9.0, title, max, if on { t.text } else { t.text2.mix(t.text, hv) });
            self.text_fit(c, self.f.sans, 12.0, ix + 28.0, cy + 10.0, desc, max, t.muted);
            if hov {
                self.set_tip(tip, "");
                if !on {
                    self.set_cursor(ponzik_window::Cursor::Hand);
                }
            }
            if hov && input.pressed && !on {
                self.lab_checkpoint("How to cut");
                if let Some(doc) = self.lab.doc.as_mut() {
                    doc.chop = mode;
                }
                if mode == Chop::Grid && self.lab.doc.as_ref().is_some_and(|d| d.bpm.is_none()) {
                    let bpm = self.project.bpm;
                    if let Some(doc) = self.lab.doc.as_mut() {
                        doc.bpm = Some(bpm);
                    }
                }
                self.lab_rechop();
            }
        }
    }

    /// Bulle « Click a piece to hear it », jusqu'à la première écoute.
    fn lab_hint(&mut self, c: &mut Canvas, wave: Rect) {
        let t = self.theme;
        let show = !self.lab.heard && self.lab.grab.is_none();
        let a = self.anim.to(wid("lab-hint", 0, 0), show as u8 as f32, T_LAYOUT);
        if a < 0.01 {
            return;
        }
        let label = "Click a piece to hear it";
        let w = self.fonts.measure(self.f.sans_med, 13.0, label) + 46.0;
        let (cx, _) = wave.center();
        let b = Rect::new((cx - w * 0.5).round(), (wave.y + 30.0 - (1.0 - a) * 6.0).round(), w, 34.0);
        c.fill_rrect(b.translate(0.0, 4.0), 17.0, Color::rgba(0, 0, 0, (80.0 * a) as u8));
        c.fill_rrect(b, 17.0, t.accent.alpha(a));
        let mut tri = Shape::new();
        tri.polygon(&[(cx - 7.0, b.bottom() - 1.0), (cx + 7.0, b.bottom() - 1.0), (cx, b.bottom() + 7.0)]);
        c.fill_path(&tri, t.accent.alpha(a));
        draw_icon(c, Icon::Play, b.x + 20.0, b.y + 17.0, 13.0, t.on_accent.alpha(a));
        self.text(c, self.f.sans_med, 13.0, b.x + 34.0, b.y + 17.0, label, t.on_accent.alpha(a));
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
            let col = if inside { piece_color(&t, k, false).alpha(0.7) } else { t.muted.alpha(0.3) };
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
            self.set_tip("The whole sound\nClick or drag to move the zoomed view", "");
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
                self.set_tip("Loop a part: drag along this edge (it snaps to the pieces)\nSpace plays the loop · right-click removes it", "");
            } else {
                self.set_tip("Click to play from here · drag to scrub through the sound", "Space");
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
            let a = 0.03 + 0.07 * sel + 0.14 * flash;
            c.fill_rect(Rect::new(xa, r.y, xb - xa, r.h), piece_color(&t, k, false).alpha(a));
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
                let col = if spectral { bright_color(&t, br) } else { piece_color(&t, k, k == selected) };
                let col = col.alpha(if k == selected { 0.95 } else { 0.78 });
                c.fill_rect(Rect::new(x, mid - h.min(1.0) * amp, 1.0, ((h.min(1.0) - l.max(-1.0)) * amp).max(1.0)), col);
                if h > 1.0 || l < -1.0 {
                    // Écrêtage après le gain : en rouge.
                    c.fill_rect(Rect::new(x, mid - amp - 2.0, 1.0, 3.0), t.negative);
                    c.fill_rect(Rect::new(x, mid + amp - 1.0, 1.0, 3.0), t.negative);
                }
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
            let col = piece_color(&t, k, false);
            let hv = self.anim.get(wid("lab-mk-h", m, 0)).unwrap_or(0.0);
            c.fill_rect(Rect::new(x - 0.5, r.y, 1.5 + hv, r.h), col.alpha(0.65 + 0.35 * hv));
        }
        for (k, &m) in bounds.iter().enumerate().take(bounds.len() - 1) {
            let x = x_of(m as f64).round();
            if x < r.x - 30.0 || x > r.right() {
                continue;
            }
            let col = piece_color(&t, k, k == selected);
            let label = format!("{}", k + 1);
            let w = self.fonts.measure(self.f.mono_med, 11.0, &label) + 10.0;
            let sel = k == selected;
            // Tranche trop étroite pour son numéro : un simple fanion (sauf la sélection).
            let room = x_of(bounds[k + 1] as f64) - x;
            let flag = if room >= w + 2.0 || sel { Rect::new(x, r.y + 4.0, w, 16.0) } else { Rect::new(x, r.y + 4.0, room.clamp(3.0, 6.0), 10.0) };
            c.fill_rrect(flag, if flag.w < 8.0 { 1.5 } else { 4.0 }, if sel { col } else { col.mix(t.bg, 0.55) });
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
        let edit = self.lab.edit_open;
        if hov_wave && self.lab.grab.is_none() && edit {
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
        for (g, x, y) in handles.into_iter().filter(|_| edit) {
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
            if !on || !edit {
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
                Some(Grab::FadeIn) => "Fade in: drag to make the start come in softly",
                Some(Grab::FadeOut) => "Fade out: drag to make the end die away",
                Some(Grab::CurveIn | Grab::CurveOut) => "Fade shape: drag up or down",
                Some(Grab::TrimStart) => "Start of the sound: drag to cut off the beginning",
                Some(Grab::TrimEnd) => "End of the sound: drag to cut off the end",
                _ if hover_marker.is_some() => "A cut: drag to move it · hold right-click to remove it",
                _ if doc.chop == Chop::Manual => "Click to add a cut here · hold right-click on a cut to remove it\nScroll to zoom in · Shift+scroll to move along",
                _ => "Click a piece to hear it · double-click to add a cut\nScroll to zoom in · Shift+scroll to move along",
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
            self.lab_checkpoint("Remove cuts");
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
                self.lab_checkpoint("Move a cut");
                self.lab_go_manual();
                self.lab.grab = Some(Grab::Marker(i));
            } else {
                let f = f_at(input.x) as usize;
                let k = bounds.partition_point(|&m| m <= f).saturating_sub(1).min(bounds.len() - 2);
                let add = (doc.chop == Chop::Manual || input.double) && f > ta + MIN_GAP && f + MIN_GAP < tb && doc.markers.len() < MAX_SLICES - 1;
                if add {
                    let at = self.lab_zero_crossing(f);
                    self.lab_checkpoint("Add a cut");
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
            let col = piece_color(&t, k, k == selected);
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
                self.set_tip(&format!("Piece {} · {} · plays on the key {}\nClick to hear it · drag it into the playlist or the channel rack to use only this piece", k + 1, fmt_secs(secs), note_name(60 + k as u8)), "← →");
                self.set_cursor(ponzik_window::Cursor::Hand);
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

    // --- Étape « Use » ---

    fn lab_use_step(&mut self, c: &mut Canvas, input: &Input, main: Rect) {
        let t = self.theme;
        let Some(n) = self.lab.doc.as_ref().map(Doc::slices) else { return };
        let source = self.lab.source;
        let mut y = main.y;
        let title = format!("Your sound is cut into {n} piece{}. What now?", if n == 1 { "" } else { "s" });
        self.text(c, self.f.sans_med, 18.0, main.x + 4.0, y + 12.0, &title, t.text);
        let sub = "Pick one of the two below. You can go back to Cut and change the pieces at any time.";
        self.text_fit(c, self.f.sans, 13.0, main.x + 4.0, y + 36.0, sub, main.w - 8.0, t.text2);
        y += 56.0;
        if self.lab.done.is_some() {
            self.lab_banner(c, input, Rect::new(main.x, y, main.w, 60.0));
            y += 70.0;
        }
        self.lab_strip(c, input, Rect::new(main.x, y, main.w, 72.0));
        y += 84.0;

        // Les deux grandes cartes.
        let (gap, card_h) = (14.0, 150.0);
        let w = (main.w - gap) / 2.0;
        let a = Rect::new(main.x, y, w, card_h);
        let pads = (
            Icon::Rack,
            "Play the pieces on pads",
            "Adds a Slicer to your channel rack. Each piece gets its own key, from C4 up: play them with your computer keyboard or in the piano roll.",
            "Create pads",
            "Creates a new Slicer channel\nPiece 1 plays on C4, piece 2 on C#4, and so on",
        );
        if self.lab_card(c, input, 0, a, pads) {
            self.lab_to_slicer(false);
        }
        let flip = (
            Icon::Piano,
            "Rebuild the loop as a pattern",
            "Same pads, plus a pattern that plays the pieces in their original order. Then rearrange the steps to make your own beat.",
            "Create the pattern",
            "Creates a Slicer channel and a new pattern\nThe pattern plays the pieces in order: you hear the original loop",
        );
        if self.lab_card(c, input, 1, Rect::new(a.right() + gap, y, w, card_h), flip) {
            self.lab_to_slicer(true);
        }
        y += card_h + 18.0;

        // Quelle touche joue quelle tranche.
        self.text(c, self.f.sans_med, 13.0, main.x + 4.0, y + 8.0, "Which key plays which piece", t.text2);
        self.lab_keys(c, input, Rect::new(main.x, y + 22.0, main.w, 62.0));
        y += 98.0;

        // Actions secondaires.
        let y = y.min(main.bottom() - 36.0);
        let mut items = vec![
            (Icon::Save, "Save the pieces as files", "Saves every piece as its own WAV file\nin the Échantillons folder, ready for any sampler"),
            (Icon::Export, "Save as one file", "Saves the whole edited sound as one WAV file\nThe cuts are kept inside the file"),
        ];
        if source != Source::Browser {
            items.push((Icon::Check, "Replace the original sample", "Puts your edited version back where the sound came from\nCtrl+Z brings the old sound back"));
        }
        let mut x = main.x;
        for (i, (icon, label, tip)) in items.into_iter().enumerate() {
            let bw = self.button_width(true, label);
            if self.lab_button(c, input, wid("lab-second", i, 0), Rect::new(x, y, bw, 36.0), Some(icon), label, 0, tip) {
                match i {
                    0 => self.lab_save_pieces(),
                    1 => self.lab_save_action(),
                    _ => self.lab_apply(),
                }
            }
            x += bw + 10.0;
        }
    }

    /// Grande carte d'action : icône, titre, phrase, bouton.
    fn lab_card(&mut self, c: &mut Canvas, input: &Input, i: usize, r: Rect, (icon, title, desc, action, tip): (Icon, &str, &str, &str, &str)) -> bool {
        let t = self.theme;
        let id = wid("lab-card", i, 0);
        let hov = self.hovered(input, id, r);
        let hv = self.hover_anim(id, hov);
        let press = self.anim.to(id ^ 0x5052, if hov && input.down { 0.98 } else { 1.0 }, T_PRESS);
        let r = r.scale(press);
        c.fill_rrect(r, 16.0, t.panel.mix(t.raised, hv));
        c.stroke_rrect(r, 16.0, 1.0, t.hairline.mix(t.accent.alpha(0.6), hv));
        let (ix, iy) = (r.x + 40.0, r.y + 40.0);
        c.fill_circle(ix, iy, 22.0, t.accent.alpha(0.15 + 0.1 * hv));
        draw_icon(c, icon, ix, iy, 20.0, t.accent);
        let x0 = r.x + 78.0;
        let max = r.right() - 20.0 - x0;
        self.text_fit(c, self.f.sans_med, 16.0, x0, r.y + 30.0, title, max, t.text);
        for (k, line) in self.wrap(self.f.sans, 13.0, desc, max).iter().take(3).enumerate() {
            self.text(c, self.f.sans, 13.0, x0, r.y + 54.0 + k as f32 * 19.0, line, t.text2);
        }
        let aw = self.button_width(true, action) + 4.0;
        let b = Rect::new(x0, r.bottom() - 48.0, aw, 34.0);
        c.fill_rrect(b, 10.0, t.accent.mix(t.accent_hot, hv * 0.2));
        self.text(c, self.f.sans_med, 13.0, b.x + 16.0, b.y + 17.0, action, t.on_accent);
        draw_icon(c, Icon::ChevronRight, b.right() - 18.0 + 2.0 * hv, b.y + 17.0, 14.0, t.on_accent);
        if hov {
            self.set_tip(tip, "");
            self.set_cursor(ponzik_window::Cursor::Hand);
        }
        hov && input.pressed
    }

    /// Confirmation après une action, avec « Go to it ».
    fn lab_banner(&mut self, c: &mut Canvas, input: &Input, r: Rect) {
        let t = self.theme;
        let Some((text, detail, go)) = self.lab.done.as_ref().map(|d| (d.text.clone(), d.detail.clone(), d.go)) else { return };
        let a = self.anim.to(wid("lab-done", 0, 0), 1.0, T_LAYOUT);
        let r = r.translate(0.0, ((1.0 - a) * 6.0).round());
        c.fill_rrect(r, 14.0, t.panel.mix(t.accent, 0.12 * a));
        c.stroke_rrect(r, 14.0, 1.0, t.accent.alpha(0.55 * a));
        let cy = r.y + r.h * 0.5;
        c.fill_circle(r.x + 30.0, cy, 14.0, t.accent);
        draw_icon(c, Icon::Check, r.x + 30.0, cy, 15.0, t.on_accent);
        let close = Rect::new(r.right() - 44.0, cy - 16.0, 32.0, 32.0);
        if self.icon_button(c, input, wid("lab-done-x", 0, 0), close, Icon::Close, false, ("Hide this message", "")) {
            self.lab.done = None;
        }
        let mut right = close.x - 8.0;
        if go != Go::Nothing {
            let label = "Go to it";
            let w = self.button_width(true, label) + 4.0;
            let b = Rect::new(right - w, cy - 19.0, w, 38.0);
            let tip = match go {
                Go::Pattern(..) => "Open the new pattern in the piano roll",
                Go::Playlist => "Open the playlist",
                _ => "Open the channel rack on the new channel",
            };
            if self.lab_button(c, input, wid("lab-go", 0, 0), b, Some(Icon::ChevronRight), label, 3, tip) {
                self.lab_go(go);
            }
            right = b.x - 16.0;
        }
        let max = right - (r.x + 56.0);
        self.text_fit(c, self.f.sans_med, 14.0, r.x + 56.0, cy - 9.0, &text, max, t.text);
        self.text_fit(c, self.f.sans, 12.0, r.x + 56.0, cy + 11.0, &detail, max, t.text2);
    }

    /// Le son entier en bande, tranche par tranche : clic pour écouter.
    fn lab_strip(&mut self, c: &mut Canvas, input: &Input, r: Rect) {
        let t = self.theme;
        let Some(doc) = self.lab.doc.clone() else { return };
        let Some(cache) = self.lab.cache.as_ref() else { return };
        c.fill_rrect(r, 12.0, t.well);
        let g = r.inset_xy(12.0, 10.0);
        let (a, b) = doc.trim;
        let span = (b - a).max(1) as f32;
        let bounds = doc.bounds();
        let n = bounds.len() - 1;
        let selected = self.lab.selected.min(n - 1);
        let id = wid("lab-strip", 0, 0);
        let hov = self.hovered(input, id, r);
        let f_at = |x: f32| a + (((x - g.x) / g.w).clamp(0.0, 0.9999) * span) as usize;
        let x_of = |f: usize| g.x + (f.saturating_sub(a)) as f32 / span * g.w;
        let hk = hov.then(|| bounds.partition_point(|&m| m <= f_at(input.x)).saturating_sub(1).min(n - 1));
        let mid = g.y + g.h * 0.5 + 6.0;
        let amp = g.h * 0.5 - 6.0;
        let np = cache.peaks.len();
        let mut cols = Vec::with_capacity(g.w as usize);
        for px in 0..g.w as usize {
            let fa = f_at(g.x + px as f32);
            let fb = f_at(g.x + px as f32 + 1.0).max(fa + 1);
            let (ba, bb) = ((fa / BUCKET).min(np.saturating_sub(1)), (fb / BUCKET).clamp(fa / BUCKET + 1, np.max(1)));
            let (lo, hi) = cache.peaks.get(ba..bb).map_or((0.0, 0.0), |w| w.iter().fold((0.0f32, 0.0f32), |(l, h), &(x, y)| (l.min(x), h.max(y))));
            let e = doc.envelope(fa);
            let k = bounds.partition_point(|&m| m <= fa).saturating_sub(1).min(n - 1);
            let lit = k == selected || hk == Some(k);
            let col = piece_color(&t, k, k == selected).alpha(if lit { 0.95 } else { 0.7 });
            cols.push((Rect::new(g.x + px as f32, mid - (hi * e).min(1.0) * amp, 1.0, (((hi - lo) * e).min(2.0) * amp).max(1.0)), col));
        }
        for (rect, col) in cols {
            c.fill_rect(rect, col);
        }
        for &m in &bounds[1..n] {
            c.fill_rect(Rect::new(x_of(m).round() - 1.0, g.y, 2.0, g.h), t.well);
        }
        for k in 0..n {
            let (xa, xb) = (x_of(bounds[k]), x_of(bounds[k + 1]));
            let label = format!("{}", k + 1);
            if xb - xa >= self.fonts.measure(self.f.mono_med, 11.0, &label) + 8.0 {
                let col = if k == selected || hk == Some(k) { t.text } else { t.muted };
                self.text(c, self.f.mono_med, 11.0, xa + 4.0, g.y + 5.0, &label, col);
            }
        }
        if let Some(p) = self.lab_playhead() {
            let x = x_of(p as usize).round();
            if x >= g.x && x <= g.right() {
                c.fill_rect(Rect::new(x - 1.0, g.y, 2.0, g.h), t.select);
            }
        }
        if let Some(k) = hk {
            let secs = (bounds[k + 1] - bounds[k]) as f32 / doc.sample.sample_rate.max(1.0);
            self.set_tip(&format!("Piece {} · {}\nClick to hear it", k + 1, fmt_secs(secs)), "← →");
            self.set_cursor(ponzik_window::Cursor::Hand);
            if input.pressed {
                self.lab_play_slice(k);
            }
        }
    }

    /// Clavier : chaque tranche sur sa touche, à partir de C4.
    fn lab_keys(&mut self, c: &mut Canvas, input: &Input, r: Rect) {
        let t = self.theme;
        let Some(n) = self.lab.doc.as_ref().map(Doc::slices) else { return };
        let selected = self.lab.selected.min(n - 1);
        let live = self.lab_playhead().and_then(|p| {
            let b = self.lab.doc.as_ref()?.bounds();
            let k = b.partition_point(|&m| m as f64 <= p).checked_sub(1)?;
            (k < n).then_some(k)
        });
        let black = |note: usize| matches!(note % 12, 1 | 3 | 6 | 8 | 10);
        let first = 60usize;
        let mut last = first + n - 1;
        while black(last) {
            last += 1;
        }
        let whites = (first..=last).filter(|&k| !black(k)).count().max(1);
        let ww = (r.w / whites as f32).min(46.0);
        let bw = ww * 0.62;
        let mut keys: Vec<(usize, Rect, bool)> = Vec::new();
        let mut wx = r.x;
        for note in first..=last {
            if black(note) {
                keys.push((note, Rect::new(wx - bw * 0.5, r.y, bw, r.h * 0.6), true));
            } else {
                keys.push((note, Rect::new(wx, r.y, ww - 2.0, r.h), false));
                wx += ww;
            }
        }
        let hit = keys.iter().filter(|k| k.2).chain(keys.iter().filter(|k| !k.2)).find(|(_, kr, _)| input.inside && kr.contains(input.x, input.y)).map(|k| k.0);
        let id = wid("lab-keys", 0, 0);
        let hov = self.hovered(input, id, r);
        for pass_black in [false, true] {
            for &(note, kr, is_black) in keys.iter().filter(|k| k.2 == pass_black) {
                let k = note - first;
                let used = k < n;
                let lit = used && (live == Some(k) || (hov && hit == Some(note)));
                let col = piece_color(&t, k, k == selected);
                let base = if is_black { t.bg_deep } else { t.raised_hi };
                let fill = if !used { base.mix(t.bg, 0.5) } else if lit { base.mix(col, 0.45) } else if k == selected { base.mix(col, 0.22) } else { base };
                c.fill_rrect(kr, if is_black { 4.0 } else { 6.0 }, fill);
                if is_black {
                    c.stroke_rrect(kr, 4.0, 1.0, t.bg);
                }
                if used {
                    c.fill_rrect(Rect::new(kr.x + 3.0, kr.bottom() - 5.0, kr.w - 6.0, 3.0), 1.5, col.alpha(0.9));
                    let label = format!("{}", k + 1);
                    let size = if is_black { 10.0 } else { 12.0 };
                    let f = if is_black { self.f.mono } else { self.f.mono_med };
                    let tc = if k == selected || lit { t.text } else { t.text2 };
                    self.text_center(c, f, size, kr.x + kr.w * 0.5, kr.bottom() - 15.0, &label, tc);
                }
                if !is_black && note % 12 == 0 {
                    self.text(c, self.f.sans, 10.0, kr.x + 5.0, kr.y + 9.0, &note_name(note as u8), t.muted);
                }
            }
        }
        if hov && let Some(note) = hit {
            let k = note - first;
            if k < n {
                self.set_tip(&format!("Piece {} plays on the key {}\nClick to hear it", k + 1, note_name(note as u8)), "");
                self.set_cursor(ponzik_window::Cursor::Hand);
                if input.pressed {
                    self.lab_play_slice(k);
                }
            }
        }
    }

    // --- Tiroir « Edit sound » ---

    fn lab_drawer(&mut self, c: &mut Canvas, input: &Input, r: Rect) {
        let t = self.theme;
        let Some(doc) = self.lab.doc.clone() else { return };
        c.fill_rrect(r, 14.0, t.panel);
        c.stroke_rrect(r, 14.0, 1.0, t.hairline);
        let x = r.x + 16.0;
        let w = r.w - 32.0;
        let mut y = r.y + 14.0;
        self.text(c, self.f.sans_med, 15.0, x, y + 12.0, "Edit sound", t.text);
        let close = Rect::new(r.right() - 44.0, y - 2.0, 30.0, 28.0);
        if self.icon_button(c, input, wid("lab-drawer-x", 0, 0), close, Icon::Close, false, ("Close Edit sound", "")) {
            self.lab.edit_open = false;
        }
        self.tip_zone(input, Rect::new(x, y, 120.0, 26.0), "Edit sound\nEvery change can be undone with Ctrl+Z", "");
        y += 36.0;
        let section = |app: &mut App, c: &mut Canvas, y: &mut f32, title: &str| {
            c.fill_rect(Rect::new(x, *y, w, 1.0), t.hairline);
            app.text(c, app.f.sans_med, 12.0, x, *y + 16.0, title, t.text2);
            *y += 28.0;
        };
        let sr = doc.sample.sample_rate.max(1.0);
        let bounds = doc.bounds();
        let k = self.lab.selected.min(bounds.len() - 2);

        // Volume.
        section(self, c, &mut y, "Volume");
        let gain = doc.gain_db;
        let text = format!("{}{} dB", if gain > 0.05 { "+" } else { "" }, fr(gain, 1));
        let norm = (gain + 24.0) / 36.0;
        if let Some(v) = self.lab_slider(c, input, wid("lab-gain", 0, 0), Rect::new(x, y, w - 42.0, 34.0), "Louder or quieter", norm, &text, 24.0 / 36.0, "Makes the whole sound louder or quieter") {
            self.lab_checkpoint("Volume");
            if let Some(d) = self.lab.doc.as_mut() {
                d.gain_db = (v * 36.0 - 24.0).clamp(-24.0, 12.0);
            }
        }
        let nb = Rect::new(x + w - 34.0, y + 2.0, 34.0, 32.0);
        if self.icon_button(c, input, wid("lab-norm", 0, 0), nb, Icon::Normalize, false, ("As loud as possible\nRaises the volume until the loudest point just fits, without distorting", "")) {
            self.lab_normalize();
        }
        y += 46.0;

        // Fondus.
        section(self, c, &mut y, "Fades");
        let half = ((doc.trim.1 - doc.trim.0) / 2).max(1);
        let max_fade = half.min((sr * 2.0) as usize) as f32;
        let fades = [
            ("Fade in", doc.fade_in, "Makes the start come in softly\nYou can also drag the small square at the top left of the wave"),
            ("Fade out", doc.fade_out, "Makes the end die away softly\nYou can also drag the small square at the top right of the wave"),
        ];
        for (i, (label, frames, tip)) in fades.into_iter().enumerate() {
            let norm = (frames as f32 / max_fade).sqrt();
            let text = fmt_secs(frames as f32 / sr);
            if let Some(v) = self.lab_slider(c, input, wid("lab-fade", i, 0), Rect::new(x, y, w, 34.0), label, norm, &text, 0.0, tip) {
                self.lab_checkpoint("Fade");
                let f = (v * v * max_fade) as usize;
                if let Some(d) = self.lab.doc.as_mut() {
                    if i == 0 { d.fade_in = f } else { d.fade_out = f }
                }
            }
            y += 42.0;
        }
        y += 4.0;

        // Tempo.
        section(self, c, &mut y, "Tempo");
        let field = Rect::new(x, y, w - 42.0, 34.0);
        let bpm = doc.bpm;
        let text = bpm.map_or("?".to_string(), |b| fr(b, 1));
        if let Some(v) = self.drag_field(c, input, wid("lab-bpm", 0, 0), field, bpm.unwrap_or(self.project.bpm), 0.1, 1.0, &text, "BPM of the loop") {
            self.lab_checkpoint("Loop tempo");
            if let Some(d) = self.lab.doc.as_mut() {
                d.bpm = Some(((v * 10.0).round() / 10.0).clamp(30.0, 300.0));
            }
            if doc.chop == Chop::Grid {
                self.lab_rechop();
            }
        }
        self.tip_zone(input, field, "Tempo of the loop, in beats per minute\nDrag up or down, or scroll (Shift: fine)", "");
        let det = Rect::new(x + w - 34.0, y + 1.0, 34.0, 32.0);
        if self.icon_button(c, input, wid("lab-detect", 0, 0), det, Icon::Metronome, false, ("Find the tempo automatically", "")) {
            self.lab_detect_bpm();
        }
        y += 44.0;
        let pbpm = self.project.bpm;
        draw_icon(c, Icon::Stretch, x + 7.0, y + 6.0, 14.0, t.muted);
        self.text(c, self.f.sans, 12.0, x + 20.0, y + 6.0, &format!("Match project tempo ({} BPM)", fr(pbpm, 1)), t.text2);
        y += 18.0;
        let half_w = (w - 8.0) / 2.0;
        let keep = "Match project tempo, keep the pitch\nThe loop gets faster or slower but keeps its notes";
        if self.lab_button(c, input, wid("lab-stretch", 0, 0), Rect::new(x, y, half_w, 34.0), None, "Keep pitch", 0, keep) {
            self.lab_fit_tempo(true);
        }
        let record = "Match project tempo like a turntable\nFaster also sounds higher, slower sounds lower";
        if self.lab_button(c, input, wid("lab-speed", 0, 0), Rect::new(x + half_w + 8.0, y, half_w, 34.0), None, "Like a record", 0, record) {
            self.lab_fit_tempo(false);
        }
        y += 48.0;

        // Hauteur et sens : tout le son, ou la tranche choisie.
        section(self, c, &mut y, "Pitch and direction");
        let piece = format!("Piece {}", k + 1);
        let tips = [("Apply to the whole sound", ""), ("Apply only to the selected piece\nClick a piece on the wave to select it", "")];
        let sel = self.lab.edit_piece as usize;
        if let Some(i) = self.segmented_tips(c, input, wid("lab-scope", 0, 0), Rect::new(x, y, w, 30.0), &["Whole sound", &piece], &tips, sel) {
            self.lab.edit_piece = i == 1;
        }
        y += 38.0;
        let on_piece = self.lab.edit_piece;
        let items = if on_piece {
            [
                ("Pitch down", "This piece one semitone lower\nIt also gets a little longer"),
                ("Pitch up", "This piece one semitone higher\nIt also gets a little shorter"),
                ("Play backwards", "Plays this piece backwards"),
            ]
        } else {
            [
                ("Pitch down", "The whole sound one semitone lower\nThe length stays the same"),
                ("Pitch up", "The whole sound one semitone higher\nThe length stays the same"),
                ("Play backwards", "Plays the whole sound backwards"),
            ]
        };
        for (i, (label, tip)) in items.into_iter().enumerate() {
            let b = match i {
                0 => Rect::new(x, y, half_w, 34.0),
                1 => Rect::new(x + half_w + 8.0, y, half_w, 34.0),
                _ => Rect::new(x, y + 42.0, w, 34.0),
            };
            if self.lab_button(c, input, wid("lab-pitch", i, 0), b, None, label, 0, tip) {
                match (on_piece, i) {
                    (true, 0) => self.lab_pitch_slice(k, -1.0),
                    (true, 1) => self.lab_pitch_slice(k, 1.0),
                    (true, _) => self.lab_reverse_slice(k),
                    (false, 0) => self.lab_transpose(-1.0),
                    (false, 1) => self.lab_transpose(1.0),
                    (false, _) => self.lab_reverse_all(),
                }
            }
        }
        y += 86.0;
        let hint = "To cut off the start or the end, drag the brackets at both ends of the wave.";
        let lines = self.wrap(self.f.sans, 12.0, hint, w);
        if y + lines.len() as f32 * 17.0 <= r.bottom() - 4.0 {
            for line in lines {
                self.text(c, self.f.sans, 12.0, x, y + 6.0, &line, t.muted);
                y += 17.0;
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
