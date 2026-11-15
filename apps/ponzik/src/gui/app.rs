//! État de l'interface, navigation, barre de transport, navigateur, menus.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use ponzik_engine::mixer::{MASTER, TRACKS};
use ponzik_engine::registry;
use ponzik_engine::{Command, EngineHandle, PlayMode, Sample, TICKS_PER_BAR, TICKS_PER_STEP};
use ponzik_project::{EffectDef, InstrumentDef, Project};
use ponzik_ui::{Anim, Canvas, Color, FontId, Fonts, Rect, embedded};
use ponzik_window::Key;

use super::Input;
use super::compat;
use super::theme::*;
use super::widgets::{Icon, draw_icon, wid};

const SCOPE_LEN: usize = 8_192;
/// Échantillons par paquet de crêtes des formes d'onde.
pub const WAVE_BUCKET: usize = 256;
const HISTORY_CAP: usize = 200;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum View {
    Rack,
    Piano,
    Playlist,
    Session,
    Mixer,
}

impl View {
    pub const ALL: [View; 5] = [View::Rack, View::Piano, View::Playlist, View::Session, View::Mixer];

    pub fn label(self) -> &'static str {
        match self {
            View::Rack => "Channel Rack",
            View::Piano => "Piano roll",
            View::Playlist => "Playlist",
            View::Session => "Session",
            View::Mixer => "Mixer",
        }
    }

    fn index(self) -> usize {
        View::ALL.iter().position(|v| *v == self).unwrap_or(0)
    }
}

/// Ce que montre le panneau d'appareils du bas.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    Channel(usize),
    Track(usize),
}

#[derive(Clone, Debug)]
pub enum Popup {
    /// Choix d'un instrument : nouveau canal, ou remplacement de celui d'un canal.
    AddInstrument { replace: Option<usize> },
    AddEffect { track: usize },
    Patterns,
    OpenProject(Vec<PathBuf>),
    ChannelMenu(usize),
    TrackMenu(usize),
    /// Destination de la sortie d'une piste.
    Output(usize),
    /// Destination d'un nouvel envoi.
    Send(usize),
    Rename { target: Rename, text: String },
    /// Palette touche-à-tout (Ctrl+K).
    Palette { query: String, selected: usize },
    /// Choix du pattern d'une case vide de la vue Session.
    SessionAssign { col: usize, row: usize },
    /// Menu d'un paramètre (clic droit sur un potard).
    Param { target: ponzik_engine::AutoTarget, value: f32, default: f32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rename {
    Project,
    Channel(usize),
    Pattern(usize),
    Track(usize),
}

/// Ce que fait une ligne de menu ou de palette.
#[derive(Clone, Debug)]
pub enum Action {
    None,
    AddInstrument(&'static str, Option<usize>),
    AddEffect(usize, &'static str),
    SelectPattern(usize),
    NewPattern,
    DuplicatePattern,
    DeletePattern,
    Open(PathBuf),
    Popup(Popup),
    DuplicateChannel(usize),
    DeleteChannel(usize),
    ChannelPianoRoll(usize),
    SetOutput(usize, usize),
    AddSend(usize, usize),
    ResetTrack(usize),
    PlayStop,
    ToggleMode,
    ToggleMetronome,
    ToggleRecord,
    ToggleTyping,
    SetView(View),
    ToggleSidebar,
    TogglePanel,
    FocusChannel(usize),
    Save,
    OpenMenu,
    NewProject,
    Export,
    Undo,
    Redo,
    LoadDemo(usize),
    SetTheme(usize),
    Automate(ponzik_engine::AutoTarget, f32),
    AssignSession(usize, usize, usize),
    RemoveAutomation(ponzik_engine::AutoTarget),
    SetParamValue(ponzik_engine::AutoTarget, f32),
}

pub struct PopupState {
    pub kind: Popup,
    pub anchor: Rect,
}

/// Geste en cours : widget saisi et valeurs de départ.
#[derive(Clone, Copy, Debug)]
pub struct Drag {
    pub id: u64,
    pub y0: f32,
    pub v0: f32,
    pub fine: bool,
}

/// VU-mètre crête : montée instantanée, retombée à 20 dB/s, maintien 1,2 s.
#[derive(Default, Clone, Copy)]
pub struct Meter {
    pub level: [f32; 2],
    pub hold: [f32; 2],
    hold_age: [f32; 2],
}

impl Meter {
    fn feed(&mut self, peak: [f32; 2], dt: f32) {
        for (i, &p) in peak.iter().enumerate() {
            self.level[i] = p.max(self.level[i] * 10f32.powf(-dt));
            if p >= self.hold[i] {
                self.hold[i] = p;
                self.hold_age[i] = 0.0;
            } else {
                self.hold_age[i] += dt;
                if self.hold_age[i] > 1.2 {
                    self.hold[i] *= 10f32.powf(-1.5 * dt);
                }
            }
        }
    }

    pub fn active(&self) -> bool {
        self.level.iter().chain(&self.hold).any(|&v| v > 1e-4)
    }

    pub fn peak(&self) -> f32 {
        self.level[0].max(self.level[1])
    }
}

pub struct FontSet {
    pub sans: FontId,
    pub sans_med: FontId,
    pub mono: FontId,
    pub mono_med: FontId,
}

struct Toast {
    text: String,
    age: f32,
}

/// État du piano roll.
pub struct PianoState {
    /// Tick au bord gauche.
    pub scroll_x: f32,
    /// Note MIDI en haut de la vue.
    pub scroll_y: f32,
    /// Pixels par tick.
    pub zoom: f32,
    pub snap: u32,
    /// Dernière durée posée, réutilisée au clic suivant.
    pub last_len: u32,
    pub selected: Vec<usize>,
    pub fitted_for: Option<(usize, usize)>,
}

pub struct PlaylistState {
    pub scroll_x: f32,
    /// Pixels par mesure.
    pub zoom: f32,
    pub brush: usize,
    pub selected: Option<usize>,
}

pub struct App {
    pub engine: EngineHandle,
    pub project: Project,
    /// État que le moteur a reçu : sert de base aux différences et à l'annulation.
    synced: Project,
    past: Vec<(String, Project)>,
    future: Vec<(String, Project)>,
    last_edit: Option<(String, f32)>,
    pub modified: bool,
    pub project_path: Option<PathBuf>,

    pub view: View,
    pub focus: Focus,
    pub sidebar: bool,
    pub panel: bool,
    pub popup: Option<PopupState>,
    toasts: Vec<Toast>,
    pub piano: PianoState,
    pub playlist: PlaylistState,
    pub sidebar_scroll: f32,
    /// Recherche du navigateur et son champ actif.
    pub sidebar_query: String,
    pub sidebar_focus: bool,
    pub chain_scroll: f32,
    pub mixer_scroll: f32,
    pub samples: HashMap<String, Arc<Sample>>,
    /// Crêtes (min, max) par paquets d'échantillons, pour dessiner les formes d'onde.
    waveforms: HashMap<String, Arc<Vec<(f32, f32)>>>,
    pub sample_files: Vec<PathBuf>,

    pub playing: bool,
    pub metronome: bool,
    /// Position de lecture en ticks.
    pub position: f64,
    pub channel_meters: Vec<Meter>,
    pub track_meters: Vec<Meter>,
    pub scope: Vec<f32>,
    pub scope_at: usize,
    pub load: f32,

    pub anim: Anim,
    pub drag: Option<Drag>,
    pub fonts: Fonts,
    pub f: FontSet,
    pub theme: Theme,
    pub sample_rate: u32,
    pub time: f32,
    melodic: HashMap<String, bool>,
    pub piano_drag: Option<super::piano::PianoDrag>,
    pub clip_drag: Option<super::playlist::ClipDrag>,
    /// Note de pré-écoute à relâcher : (canal, note).
    pub preview: Option<(usize, u8)>,
    /// Notes tenues au clavier d'ordinateur ou au contrôleur.
    pub held: Vec<super::keys::HeldNote>,
    pub keyboard_octave: i32,
    /// Enregistrement armé : les notes jouées pendant la lecture sont écrites.
    pub record: bool,
    /// État des colonnes de la vue Session (non enregistré).
    pub session_cols: [super::session::ColumnState; ponzik_project::SESSION_COLUMNS],
    /// Clavier d'ordinateur joué comme un piano (désactivé : les lettres sont des raccourcis).
    pub typing_keyboard: bool,
    /// Marqueur de départ de la lecture, en ticks (cliqué dans une règle).
    pub start_marker: f64,
    /// Presse-papiers : notes (débuts relatifs) et clip de playlist.
    pub clipboard_notes: Vec<ponzik_engine::Note>,
    pub clipboard_clip: Option<ponzik_project::ClipDef>,
    /// Sélection de temps tirée dans une règle : vue, début, fin (ticks).
    pub time_selection: Option<(View, u32, u32)>,
    pub midi_inputs: Vec<ponzik_midi::MidiInput>,
    midi_events: Vec<ponzik_midi::MidiEvent>,
}

impl App {
    pub fn new(engine: EngineHandle, project: Project, sample_rate: u32) -> Self {
        let mut fonts = Fonts::new();
        let mut load = |data| fonts.add(data).expect("police embarquée invalide");
        let f = FontSet {
            sans: load(embedded::SANS_REGULAR),
            sans_med: load(embedded::SANS_MEDIUM),
            mono: load(embedded::MONO_REGULAR),
            mono_med: load(embedded::MONO_MEDIUM),
        };
        let channels = project.channels.len();
        Self {
            engine,
            synced: project.clone(),
            project,
            past: Vec::new(),
            future: Vec::new(),
            last_edit: None,
            modified: false,
            project_path: None,
            view: View::Rack,
            focus: Focus::Channel(0),
            sidebar: true,
            panel: true,
            popup: None,
            toasts: Vec::new(),
            piano: PianoState {
                scroll_x: 0.0,
                scroll_y: 72.0,
                zoom: 1.0,
                snap: TICKS_PER_STEP,
                last_len: TICKS_PER_STEP,
                selected: Vec::new(),
                fitted_for: None,
            },
            playlist: PlaylistState { scroll_x: 0.0, zoom: 44.0, brush: 0, selected: None },
            sidebar_scroll: 0.0,
            sidebar_query: String::new(),
            sidebar_focus: false,
            chain_scroll: 0.0,
            mixer_scroll: 0.0,
            samples: HashMap::new(),
            waveforms: HashMap::new(),
            sample_files: compat::scan_samples(),
            playing: false,
            metronome: false,
            position: 0.0,
            channel_meters: vec![Meter::default(); channels],
            track_meters: vec![Meter::default(); TRACKS],
            scope: vec![0.0; SCOPE_LEN],
            scope_at: 0,
            load: 0.0,
            anim: Anim::new(),
            drag: None,
            fonts,
            f,
            theme: PONZIK,
            sample_rate,
            time: 0.0,
            melodic: HashMap::new(),
            piano_drag: None,
            clip_drag: None,
            preview: None,
            held: Vec::new(),
            keyboard_octave: 4,
            record: false,
            typing_keyboard: false,
            session_cols: Default::default(),
            start_marker: 0.0,
            clipboard_notes: Vec::new(),
            clipboard_clip: None,
            time_selection: None,
            midi_inputs: ponzik_midi::open_all_inputs(),
            midi_events: Vec::new(),
        }
    }

    // --- Projet, moteur, historique ---

    pub fn send(&mut self, cmd: Command) {
        if let Err(cmd) = self.engine.send(cmd) {
            eprintln!("file de commandes pleine : {cmd:?}");
        }
    }

    fn sample_loader(&self) -> impl Fn(&str) -> Option<Arc<Sample>> + '_ {
        |path: &str| self.samples.get(path).cloned()
    }

    /// Envoie au moteur la différence entre ce qu'il a et le projet courant.
    fn sync_engine(&mut self) {
        let cmds = {
            let loader = self.sample_loader();
            super::sync::diff(&self.synced, &self.project, self.sample_rate as f32, &loader)
        };
        for cmd in cmds {
            self.send(cmd);
        }
        self.synced = self.project.clone();
        self.channel_meters.resize(self.project.channels.len(), Meter::default());
    }

    /// À appeler après chaque modification du projet. Les gestes continus (même
    /// libellé à moins d'une seconde) forment une seule étape d'annulation.
    pub fn commit(&mut self, label: &str) {
        if self.project == self.synced {
            return;
        }
        let merge = self.last_edit.as_ref().is_some_and(|(l, t)| l == label && self.time - t < 1.0);
        if !merge {
            self.past.push((label.to_string(), self.synced.clone()));
            if self.past.len() > HISTORY_CAP {
                self.past.remove(0);
            }
        }
        self.future.clear();
        self.last_edit = Some((label.to_string(), self.time));
        self.modified = true;
        self.sync_engine();
    }

    pub fn undo(&mut self) {
        if let Some((label, prev)) = self.past.pop() {
            let current = std::mem::replace(&mut self.project, prev);
            self.future.push((label.clone(), current));
            self.after_history(&format!("Annulé : {label}"));
        }
    }

    pub fn redo(&mut self) {
        if let Some((label, next)) = self.future.pop() {
            let current = std::mem::replace(&mut self.project, next);
            self.past.push((label.clone(), current));
            self.after_history(&format!("Rétabli : {label}"));
        }
    }

    fn after_history(&mut self, msg: &str) {
        self.last_edit = None;
        self.modified = true;
        self.clamp_selection();
        self.sync_engine();
        self.toast(msg);
    }

    fn clamp_selection(&mut self) {
        let n = self.project.channels.len();
        if let Focus::Channel(c) = self.focus
            && c >= n
        {
            self.focus = if n > 0 { Focus::Channel(n - 1) } else { Focus::Track(MASTER) };
        }
        self.project.current_pattern = self.project.current_pattern.min(self.project.patterns.len().saturating_sub(1));
        self.piano.selected.clear();
        self.playlist.selected = None;
    }

    /// Remplace tout le projet (ouverture, nouveau) : le moteur est resynchronisé.
    pub fn replace_project(&mut self, project: Project, path: Option<PathBuf>) {
        let paths: Vec<String> = project
            .channels
            .iter()
            .filter_map(|c| c.instrument.sample.clone())
            .chain(project.audio_clips.iter().map(|c| c.path.clone()))
            .collect();
        for path in paths {
            self.ensure_sample(&path);
        }
        self.session_cols = Default::default();
        self.project = project;
        self.project.session_live.clear();
        self.past.clear();
        self.future.clear();
        self.last_edit = None;
        self.project_path = path;
        self.modified = false;
        self.focus = Focus::Channel(0);
        self.clamp_selection();
        self.piano.fitted_for = None;
        self.send(Command::Stop);
        self.send(Command::Stop);
        self.playing = false;
        self.sync_engine();
    }

    pub fn toast(&mut self, text: &str) {
        self.toasts.push(Toast { text: text.to_string(), age: 0.0 });
        if self.toasts.len() > 3 {
            self.toasts.remove(0);
        }
    }

    pub fn ensure_sample(&mut self, path: &str) -> bool {
        if self.samples.contains_key(path) {
            return true;
        }
        match compat::load_sample(path) {
            Ok(s) => {
                self.samples.insert(path.to_string(), Arc::new(s));
                true
            }
            Err(e) => {
                self.toast(&e);
                false
            }
        }
    }

    /// Vrai si ce type d'instrument joue des hauteurs (mis en cache).
    pub fn is_melodic(&mut self, kind: &str) -> bool {
        if let Some(&m) = self.melodic.get(kind) {
            return m;
        }
        let m = registry::create_instrument(kind).is_some_and(|i| i.is_melodic());
        self.melodic.insert(kind.to_string(), m);
        m
    }

    /// Joue une note de pré-écoute, relâchée au prochain relâchement de souris.
    pub fn preview_note(&mut self, ch: usize, note: u8) {
        self.release_preview();
        self.send(Command::NoteOn { channel: ch, note, velocity: 0.8 });
        self.preview = Some((ch, note));
    }

    pub fn release_preview(&mut self) {
        if let Some((channel, note)) = self.preview.take() {
            self.send(Command::NoteOff { channel, note });
        }
    }

    /// Forme d'onde d'un fichier chargé : crêtes par paquets de [`WAVE_BUCKET`] échantillons.
    pub fn waveform(&mut self, path: &str) -> Option<Arc<Vec<(f32, f32)>>> {
        if let Some(w) = self.waveforms.get(path) {
            return Some(w.clone());
        }
        let s = self.samples.get(path)?;
        let peaks: Vec<(f32, f32)> = s
            .left
            .chunks(WAVE_BUCKET)
            .zip(s.right.chunks(WAVE_BUCKET))
            .map(|(l, r)| {
                l.iter().chain(r).fold((0.0f32, 0.0f32), |(lo, hi), &v| (lo.min(v), hi.max(v)))
            })
            .collect();
        let w = Arc::new(peaks);
        self.waveforms.insert(path.to_string(), w.clone());
        Some(w)
    }

    /// Pose un fichier audio dans la playlist, au marqueur de départ, sur une nouvelle ligne.
    pub fn add_audio_clip(&mut self, path: &str) {
        if !self.ensure_sample(path) {
            return;
        }
        let secs = self.samples[path].duration() as f64;
        let ticks = (secs * self.project.bpm as f64 / 60.0 * ponzik_engine::PPQ as f64).round().max(24.0) as u32;
        let name = std::path::Path::new(path).file_stem().map_or("Audio".to_string(), |s| s.to_string_lossy().into_owned());
        let used: Vec<usize> = self.project.channels.iter().map(|c| c.track).collect();
        let track = (1..TRACKS).find(|t| !used.contains(t) && !self.track_in_use(*t)).unwrap_or(1);
        self.project.tracks[track].name = name.clone();
        let lane = self.project.clips.iter().map(|c| c.lane + 1).chain(self.project.audio_clips.iter().map(|c| c.lane + 1)).max().unwrap_or(0);
        let start = (self.start_marker.max(0.0) as u32 / 96) * 96;
        self.project.audio_clips.push(ponzik_project::AudioClipDef {
            path: path.to_string(),
            lane,
            track,
            start,
            len: ticks,
            offset: 0.0,
            gain: 1.0,
            fade_in: 0.002,
            fade_out: 0.002,
        });
        self.commit("Poser un clip audio");
        self.toast(&format!("{name} posé dans la playlist (piste {track})"));
    }

    pub fn selected_channel(&self) -> Option<usize> {
        match self.focus {
            Focus::Channel(c) if c < self.project.channels.len() => Some(c),
            _ => None,
        }
    }

    /// Piste de mixer concernée par le focus.
    pub fn focused_track(&self) -> usize {
        match self.focus {
            Focus::Channel(c) => self.project.channels.get(c).map_or(MASTER, |ch| ch.track),
            Focus::Track(t) => t,
        }
    }

    pub fn add_instrument(&mut self, kind: &str) {
        let Some(info) = registry::instrument_kind(kind) else { return };
        let idx = self.project.add_channel(info.name, InstrumentDef::new(kind), 60);
        self.focus = Focus::Channel(idx);
        self.commit("Ajouter un canal");
        self.toast(&format!("{} ajouté au rack", info.name));
    }

    pub fn add_sample_channel(&mut self, path: &str) {
        if !self.ensure_sample(path) {
            return;
        }
        let name = std::path::Path::new(path)
            .file_stem()
            .map_or("Échantillon".to_string(), |s| s.to_string_lossy().into_owned());
        let mut def = InstrumentDef::new("sampler");
        def.sample = Some(path.to_string());
        if registry::instrument_kind("sampler").is_none() {
            self.toast("Le sampler n'est pas encore disponible");
            return;
        }
        let idx = self.project.add_channel(name.clone(), def, 60);
        self.focus = Focus::Channel(idx);
        self.commit("Ajouter un échantillon");
        self.toast(&format!("{name} ajouté au rack"));
    }

    pub fn add_effect(&mut self, track: usize, kind: &str) {
        let Some(info) = registry::effect_kind(kind) else { return };
        let Some(slot) = self.project.tracks[track].inserts.iter().position(Option::is_none) else {
            self.toast("Plus d'emplacement libre sur cette piste");
            return;
        };
        self.project.tracks[track].inserts[slot] = Some(EffectDef::new(kind));
        self.commit("Ajouter un effet");
        let name = self.project.tracks[track].name.clone();
        self.toast(&format!("{} ajouté sur {name}", info.name));
    }

    /// Arrête tous les clips de la vue Session.
    pub fn clear_session(&mut self) {
        self.session_cols = Default::default();
        if !self.project.session_live.is_empty() {
            self.project.session_live.clear();
            self.sync_engine();
        }
    }

    pub fn set_mode(&mut self, mode: PlayMode) {
        if self.project.mode == PlayMode::Session && mode != PlayMode::Session {
            self.clear_session();
        }
        if self.project.mode != mode {
            self.project.mode = mode;
            self.commit("Mode de lecture");
            self.send(Command::Locate(0.0));
        }
    }

    pub fn select_pattern(&mut self, p: usize) {
        let p = p.min(self.project.patterns.len() - 1);
        self.project.current_pattern = p;
        self.playlist.brush = p;
        self.piano.selected.clear();
        if let PlayMode::Pattern(_) = self.project.mode {
            self.project.mode = PlayMode::Pattern(p);
        }
        self.commit("Changer de pattern");
    }

    /// Lecture / pause : la pause garde la position.
    pub fn toggle_play(&mut self) {
        self.playing = !self.playing;
        self.send(if self.playing { Command::Play } else { Command::Stop });
    }

    /// Lecture / arrêt façon FL Studio (Espace) : l'arrêt revient au marqueur de départ.
    pub fn play_stop(&mut self) {
        if self.playing {
            self.stop();
        } else {
            self.send(Command::Locate(self.start_marker));
            self.playing = true;
            self.send(Command::Play);
        }
    }

    pub fn stop(&mut self) {
        self.release_all_held();
        self.clear_session();
        self.playing = false;
        self.send(Command::Stop);
        self.send(Command::Locate(self.start_marker));
    }

    /// Règle commune au piano roll et à la playlist : un clic place le marqueur de
    /// départ, un glisser tire une sélection de temps. Renvoie la sélection si elle
    /// vient d'être tirée ou modifiée.
    pub fn ruler(
        &mut self,
        c: &mut Canvas,
        input: &Input,
        view: View,
        ruler: Rect,
        grid: Rect,
        snap: f32,
        x_of: &dyn Fn(f32) -> f32,
        tick_at: &dyn Fn(f32) -> f32,
    ) -> Option<(u32, u32)> {
        let t = self.theme;
        let id = wid("ruler", view as usize, 0);
        let hov = self.hovered(input, id, ruler);
        let snap_t = |x: f32| ((tick_at(x).max(0.0) / snap).round() * snap) as u32;
        let mut changed = None;
        if hov && input.pressed {
            self.begin_drag(id, input, snap_t(input.x) as f32);
        }
        if let Some(d) = self.drag.filter(|d| d.id == id) {
            let (a, b) = (d.v0 as u32, snap_t(input.x));
            if a != b {
                let sel = (a.min(b), a.max(b));
                self.time_selection = Some((view, sel.0, sel.1));
                self.set_loop(view, Some(sel));
                changed = Some(sel);
            } else if input.released {
                // Simple clic : marqueur de départ, sélection et boucle effacées.
                self.time_selection = None;
                self.set_loop(view, None);
                self.set_start_marker(a as f64);
            }
            if input.released && a != b {
                self.set_start_marker(a.min(b) as f64);
            }
        }
        // Sélection de temps.
        if let Some((v, a, b)) = self.time_selection
            && v == view
        {
            let (xa, xb) = (x_of(a as f32), x_of(b as f32));
            c.fill_rect(Rect::new(xa, ruler.y, xb - xa, ruler.h), t.select.alpha(0.28));
            c.fill_rect(Rect::new(xa, grid.y, xb - xa, grid.h), t.select.alpha(0.07));
            c.fill_rect(Rect::new(xa.round(), grid.y, 1.0, grid.h), t.select.alpha(0.6));
            c.fill_rect(Rect::new(xb.round(), grid.y, 1.0, grid.h), t.select.alpha(0.6));
        }
        // Marqueur de départ : triangle dans la règle, ligne discrète à l'arrêt.
        let mx = x_of(self.start_marker as f32);
        if mx >= ruler.x - 6.0 && mx <= ruler.right() + 6.0 {
            if !self.playing {
                c.fill_rect(Rect::new(mx.round(), grid.y, 1.0, grid.h), t.text.alpha(0.35));
            }
            let mut p = ponzik_ui::Path::new();
            p.polygon(&[(mx - 6.0, ruler.y + 2.0), (mx + 6.0, ruler.y + 2.0), (mx, ruler.y + 11.0)]);
            c.fill_path(&p, t.text);
        }
        changed
    }

    /// Envoie les clips de session au moteur, sans étape d'annulation.
    pub fn sync_session(&mut self) {
        self.sync_engine();
    }

    /// La sélection de temps fait boucler la lecture quand elle correspond au mode
    /// courant (piano roll en mode pattern, playlist en mode morceau). Ce n'est pas
    /// une modification du projet : pas d'étape d'annulation.
    fn set_loop(&mut self, view: View, range: Option<(u32, u32)>) {
        let applies = matches!((view, self.project.mode), (View::Piano, PlayMode::Pattern(_)) | (View::Playlist, PlayMode::Song));
        let range = if applies { range } else { None };
        if self.project.loop_range != range {
            self.project.loop_range = range;
            self.sync_engine();
        }
    }

    /// Place le marqueur de départ (clic dans une règle).
    pub fn set_start_marker(&mut self, tick: f64) {
        self.start_marker = tick.max(0.0);
        self.send(Command::Locate(self.start_marker));
    }

    /// Notes des contrôleurs MIDI : même chemin que le clavier d'ordinateur.
    /// Appelé à chaque tour de boucle, hors rendu ; renvoie vrai s'il s'est passé quelque chose.
    pub fn poll_midi(&mut self) -> bool {
        use ponzik_midi::MidiEvent;
        let mut events = std::mem::take(&mut self.midi_events);
        for input in &self.midi_inputs {
            input.poll(&mut events);
        }
        let any = !events.is_empty();
        for ev in events.drain(..) {
            // Codes réservés aux notes MIDI, hors de l'espace des touches physiques.
            match ev {
                MidiEvent::NoteOn { channel, key, velocity } => {
                    self.live_note_on(0x1000 + channel as u16 * 128 + key as u16, key, velocity as f32 / 127.0);
                }
                MidiEvent::NoteOff { channel, key, .. } => self.live_note_off(0x1000 + channel as u16 * 128 + key as u16),
                _ => {}
            }
        }
        self.midi_events = events;
        any
    }

    // --- Fichiers ---

    pub fn save(&mut self) {
        let path = self.project_path.clone().unwrap_or_else(|| compat::default_project_path(&self.project.name));
        match compat::save_project(&self.project, &path) {
            Ok(()) => {
                self.project_path = Some(path.clone());
                self.modified = false;
                self.toast(&format!("Enregistré dans {}", path.display()));
            }
            Err(e) => self.toast(&e),
        }
    }

    pub fn open_file(&mut self, path: PathBuf) {
        match compat::load_project(&path) {
            Ok(p) => {
                let name = p.name.clone();
                self.replace_project(p, Some(path));
                self.toast(&format!("{name} ouvert"));
            }
            Err(e) => self.toast(&e),
        }
    }

    pub fn export(&mut self) {
        let path = compat::default_export_path(&self.project.name);
        let result = {
            let loader = self.sample_loader();
            compat::export_song(&self.project, self.sample_rate, &loader, &path)
        };
        match result {
            Ok(secs) => self.toast(&format!("Exporté : {} ({secs:.1} s)", path.display())),
            Err(e) => self.toast(&e),
        }
    }

    // --- Image ---

    fn poll_engine(&mut self, dt: f32) {
        self.engine.collect_garbage();
        self.position = self.engine.position();
        for (i, m) in self.channel_meters.iter_mut().enumerate() {
            m.feed(self.engine.take_channel_peak(i), dt);
        }
        for (i, m) in self.track_meters.iter_mut().enumerate() {
            m.feed(self.engine.take_track_peak(i), dt);
        }
        let load = self.engine.take_load();
        self.load += (load - self.load) * (1.0 - (-dt / 0.4).exp());
        let (scope, at) = (&mut self.scope, &mut self.scope_at);
        self.engine.drain_scope(|s| {
            scope[*at] = s;
            *at = (*at + 1) % SCOPE_LEN;
        });
    }

    /// Position dans la boucle courante, en ticks.
    pub fn cycle_position(&self) -> f64 {
        match self.project.mode {
            PlayMode::Pattern(p) => {
                let len = self.project.patterns.get(p).map_or(TICKS_PER_BAR, |p| p.len.max(1)) as f64;
                self.position.rem_euclid(len)
            }
            PlayMode::Song | PlayMode::Session => self.position,
        }
    }

    pub fn needs_redraw(&self) -> bool {
        self.playing
            || self.anim.is_moving()
            || self.drag.is_some()
            || !self.toasts.is_empty()
            || self.track_meters.iter().any(Meter::active)
            || self.channel_meters.iter().any(Meter::active)
    }

    /// Raccourcis globaux, ceux de FL Studio.
    fn shortcuts(&mut self, input: &Input) {
        for &(key, mods) in &input.keys {
            match key {
                Key::Escape => {
                    self.popup = None;
                    self.piano.selected.clear();
                    self.playlist.selected = None;
                    if let Some((v, _, _)) = self.time_selection.take() {
                        self.set_loop(v, None);
                    }
                }
                Key::Space => self.play_stop(),
                Key::F(5) => self.set_view(View::Playlist),
                Key::F(6) => self.set_view(View::Rack),
                Key::F(7) => self.set_view(View::Piano),
                Key::F(9) => self.set_view(View::Mixer),
                Key::F(8) if mods.alt => self.sidebar = !self.sidebar,
                Key::F(4) => {
                    let p = self.project.add_pattern();
                    self.select_pattern(p);
                    self.toast("Nouveau pattern");
                }
                Key::Tab => {
                    let next = View::ALL[(self.view.index() + 1) % View::ALL.len()];
                    self.set_view(next);
                }
                Key::Char('+') => {
                    let next = (self.project.current_pattern + 1).min(self.project.patterns.len() - 1);
                    self.select_pattern(next);
                }
                Key::Char('-') => self.select_pattern(self.project.current_pattern.saturating_sub(1)),
                Key::Char('z') if mods.ctrl && mods.shift => self.redo(),
                Key::Char('z') if mods.ctrl => self.undo(),
                Key::Char('y') if mods.ctrl => self.redo(),
                Key::Char('s') if mods.ctrl => self.save(),
                Key::Char('o') if mods.ctrl => {
                    self.open_popup(Popup::OpenProject(compat::list_projects()), Rect::new(12.0, TOP_BAR, 0.0, 0.0))
                }
                Key::Char('n') if mods.ctrl => {
                    self.replace_project(Project::empty(), None);
                    self.toast("Nouveau projet");
                }
                Key::Char('r') if mods.ctrl => self.export(),
                Key::Char('k') if mods.ctrl => self.open_palette(),
                Key::Char('f') if mods.ctrl => {
                    self.sidebar = true;
                    self.sidebar_focus = true;
                }
                Key::Char('m') if mods.ctrl => {
                    self.metronome = !self.metronome;
                    let m = self.metronome;
                    self.send(Command::SetMetronome(m));
                }
                Key::Char('t') if mods.ctrl => self.toggle_typing_keyboard(),
                _ if mods.ctrl || mods.alt || self.typing_keyboard => {}
                Key::Char('l') => {
                    let mode = match self.project.mode {
                        PlayMode::Pattern(_) => PlayMode::Song,
                        PlayMode::Song | PlayMode::Session => PlayMode::Pattern(self.project.current_pattern),
                    };
                    self.set_mode(mode);
                }
                Key::Char('r') => {
                    self.record = !self.record;
                    self.toast(if self.record { "Enregistrement armé" } else { "Enregistrement désarmé" });
                }
                _ => {}
            }
        }
    }

    pub fn open_palette(&mut self) {
        self.open_popup(Popup::Palette { query: String::new(), selected: 0 }, Rect::new(0.0, 0.0, 0.0, 0.0));
    }

    pub fn toggle_typing_keyboard(&mut self) {
        self.typing_keyboard = !self.typing_keyboard;
        self.release_all_held();
        self.toast(if self.typing_keyboard {
            "Clavier musical activé : les lettres jouent des notes (Ctrl+T pour revenir)"
        } else {
            "Clavier musical désactivé"
        });
    }

    pub fn set_view(&mut self, v: View) {
        if v != self.view {
            self.view = v;
            self.anim.set(wid("view-enter", 0, 0), 0.0);
        }
    }

    pub fn frame(&mut self, c: &mut Canvas, input: &Input, dt: f32) {
        self.anim.begin_frame(dt);
        self.time += dt;
        self.poll_engine(dt);
        if self.popup.is_none() && !self.sidebar_focus {
            self.shortcuts(input);
            self.musical_keyboard(input);
        } else if self.popup.is_some() && input.keys.iter().any(|(k, _)| *k == Key::Escape) {
            self.popup = None;
        }
        if input.focus_lost {
            self.release_all_held();
        }

        let t = self.theme;
        let (w, h) = (c.width() as f32, c.height() as f32);
        c.set_clip(None);
        c.clear(t.bg);

        let sb = self.anim.to(wid("sidebar", 0, 0), if self.sidebar { SIDEBAR } else { 0.0 }, T_LAYOUT).round();
        let pn = self.anim.to(wid("panel", 0, 0), if self.panel { PANEL } else { 0.0 }, T_LAYOUT).round();

        // Un menu ouvert capte la souris : le reste de l'écran ne réagit plus.
        let masked;
        let ui = if self.popup.is_some() {
            masked = Input {
                inside: false,
                pressed: false,
                right_pressed: false,
                wheel: 0.0,
                double: false,
                keys: Vec::new(),
                ..input.clone()
            };
            &masked
        } else {
            input
        };

        let main = Rect::new(sb, TOP_BAR, w - sb, h - TOP_BAR - pn);
        self.main_area(c, ui, main);
        if pn > 1.0 {
            self.panel_area(c, ui, Rect::new(sb, h - pn, w - sb, pn));
        }
        if sb > 1.0 {
            self.sidebar_area(c, ui, Rect::new(0.0, TOP_BAR, sb, h - TOP_BAR));
        }
        self.top_bar(c, ui, Rect::new(0.0, 0.0, w, TOP_BAR));
        self.popup_layer(c, input, Rect::new(0.0, 0.0, w, h));
        self.toast_layer(c, dt, Rect::new(sb, TOP_BAR, w - sb, h - TOP_BAR - pn));

        if !input.down {
            self.drag = None;
            self.release_preview();
        }
    }

    // --- Barre du haut ---

    fn top_bar(&mut self, c: &mut Canvas, input: &Input, r: Rect) {
        let t = self.theme;
        c.fill_rect(r, t.bg_deep);
        c.fill_rect(Rect::new(r.x, r.bottom() - 1.0, r.w, 1.0), t.hairline);
        let cy = r.y + r.h * 0.5;

        if self.icon_button(c, input, wid("tb-sidebar", 0, 0), Rect::new(12.0, cy - 16.0, 32.0, 32.0), Icon::Sidebar, self.sidebar) {
            self.sidebar = !self.sidebar;
        }
        // Marque : le nom et sa LED, qui s'allume quand ça joue.
        let led = self.anim.to(wid("brand-led", 0, 0), self.playing as u8 as f32, T_HEAD);
        c.fill_circle(58.0, cy, 4.0, t.raised_hi.mix(t.accent, led.max(0.35)));
        let wordmark = self.text(c, self.f.sans_med, 18.0, 70.0, cy, "Ponzik", t.text);

        // Transport, centré.
        let cluster = 40.0 + 6.0 + 40.0 + 14.0 + 264.0 + 10.0 + 188.0 + 10.0 + 116.0 + 10.0 + 132.0 + 6.0 + 36.0 + 4.0 + 36.0 + 4.0 + 36.0;
        let mut x = ((r.w - cluster) * 0.5).round().max(70.0 + wordmark + 24.0);
        let start = x;

        let play = Rect::new(x, cy - 18.0, 40.0, 36.0);
        let hov = self.hovered(input, wid("tb-play", 0, 0), play);
        if hov && input.pressed {
            self.toggle_play();
        }
        let hv = self.hover_anim(wid("tb-play", 0, 0), hov);
        let on = self.anim.to(wid("tb-play-on", 0, 0), self.playing as u8 as f32, T_HOVER);
        c.fill_rrect(play, 10.0, t.text.mix(t.accent, on).mix(WHITE, 0.12 * hv));
        c.fill_rect(Rect::new(play.x + 8.0, play.y + 1.0, play.w - 16.0, 1.0), WHITE.alpha(0.3));
        let (px, py) = play.center();
        draw_icon(c, if self.playing { Icon::Pause } else { Icon::Play }, px, py, 16.0, t.bg_deep.mix(t.on_accent, on));
        x += 46.0;
        if self.icon_button(c, input, wid("tb-stop", 0, 0), Rect::new(x, cy - 18.0, 40.0, 36.0), Icon::Stop, false) {
            self.stop();
        }
        x += 54.0;

        let mode = match self.project.mode {
            PlayMode::Pattern(_) => 0,
            PlayMode::Song => 1,
            PlayMode::Session => 2,
        };
        if let Some(m) =
            self.segmented(c, input, wid("tb-mode", 0, 0), Rect::new(x, cy - 18.0, 264.0, 36.0), &["Pattern", "Morceau", "Session"], mode)
        {
            let cur = self.project.current_pattern;
            self.set_mode(match m {
                0 => PlayMode::Pattern(cur),
                1 => PlayMode::Song,
                _ => PlayMode::Session,
            });
            if m == 2 {
                self.set_view(View::Session);
            }
        }
        x += 274.0;
        self.pattern_selector(c, input, Rect::new(x, cy - 18.0, 188.0, 36.0));
        x += 198.0;

        let bpm_text = format!("{:.1}", self.project.bpm).replace('.', ",");
        if let Some(bpm) = self.drag_field(c, input, wid("tb-bpm", 0, 0), Rect::new(x, cy - 18.0, 116.0, 36.0), self.project.bpm, 0.25, 1.0, &bpm_text, "BPM") {
            let bpm = ((bpm * 10.0).round() / 10.0).clamp(40.0, 300.0);
            if bpm != self.project.bpm {
                self.project.bpm = bpm;
                self.commit("Tempo");
            }
        }
        x += 126.0;
        self.position_field(c, Rect::new(x, cy - 18.0, 132.0, 36.0));
        x += 138.0;
        if self.icon_button(c, input, wid("tb-metro", 0, 0), Rect::new(x, cy - 18.0, 36.0, 36.0), Icon::Metronome, self.metronome) {
            self.metronome = !self.metronome;
            let m = self.metronome;
            self.send(Command::SetMetronome(m));
        }
        x += 40.0;
        let rec = Rect::new(x, cy - 18.0, 36.0, 36.0);
        let rid = wid("tb-rec", 0, 0);
        let rhov = self.hovered(input, rid, rec);
        let rhv = self.hover_anim(rid, rhov);
        let on = self.anim.to(wid("tb-rec-on", 0, 0), self.record as u8 as f32, T_HOVER);
        c.fill_rrect(rec, 8.0, t.raised.alpha(rhv.max(on * 0.6)));
        let (rx0, ry0) = rec.center();
        let pulse = if self.record && self.playing { 0.75 + 0.25 * (self.time * 6.0).sin().abs() } else { 1.0 };
        c.fill_circle(rx0, ry0, 6.0, t.muted.mix(t.text2, rhv).mix(t.negative, on).alpha(pulse));
        if rhov && input.pressed {
            self.record = !self.record;
            self.toast(if self.record { "Enregistrement armé : joue au clavier pendant la lecture" } else { "Enregistrement désarmé" });
        }
        x += 40.0;
        if self.icon_button(c, input, wid("tb-keys", 0, 0), Rect::new(x, cy - 18.0, 36.0, 36.0), Icon::Keyboard, self.typing_keyboard) {
            self.toggle_typing_keyboard();
        }
        let cluster_end = x + 36.0;

        // Droite : fichier et historique, puis la charge DSP si la place le permet.
        let mut rx = r.w - 12.0 - 32.0;
        let buttons = [
            (Icon::Search, "tb-search"),
            (Icon::Panel, "tb-panel"),
            (Icon::Export, "tb-export"),
            (Icon::Save, "tb-save"),
            (Icon::Folder, "tb-open"),
            (Icon::Redo, "tb-redo"),
            (Icon::Undo, "tb-undo"),
        ];
        for (icon, name) in buttons {
            if rx < cluster_end + 8.0 {
                break;
            }
            let on = icon == Icon::Panel && self.panel;
            if self.icon_button(c, input, wid(name, 0, 0), Rect::new(rx, cy - 16.0, 32.0, 32.0), icon, on) {
                match icon {
                    Icon::Search => self.open_palette(),
                    Icon::Panel => self.panel = !self.panel,
                    Icon::Export => self.export(),
                    Icon::Save => self.save(),
                    Icon::Folder => {
                        self.open_popup(Popup::OpenProject(compat::list_projects()), Rect::new(rx, cy - 16.0, 32.0, 32.0))
                    }
                    Icon::Redo => self.redo(),
                    Icon::Undo => self.undo(),
                    _ => {}
                }
            }
            rx -= 36.0;
        }
        let dsp = format!("DSP {:>2.0} %", (self.load * 100.0).min(999.0));
        let dsp_w = self.fonts.measure(self.f.mono, 12.0, &dsp);
        if rx + 32.0 - 16.0 - dsp_w > cluster_end + 16.0 {
            self.text_right(c, self.f.mono, 12.0, rx + 32.0 - 12.0, cy, &dsp, t.muted);
        }

        // Nom du projet entre la marque et le transport.
        let name_x = 70.0 + wordmark + 20.0;
        let room = start - name_x - 20.0;
        if room > 80.0 {
            let name = self.project.name.clone();
            let label = if self.modified { format!("{name} ·") } else { name.clone() };
            let w = self.fonts.measure(self.f.sans, 13.0, &label).min(room);
            let area = Rect::new(name_x - 6.0, cy - 14.0, w + 12.0, 28.0);
            let id = wid("tb-project", 0, 0);
            let hov = self.hovered(input, id, area);
            let hv = self.hover_anim(id, hov);
            if hv > 0.01 {
                c.fill_rrect(area, 8.0, t.raised.alpha(hv));
            }
            if hov && input.double {
                self.open_popup(Popup::Rename { target: Rename::Project, text: name }, area);
            }
            self.text_fit(c, self.f.sans, 13.0, name_x, cy, &label, room, t.text2.mix(t.text, hv));
        }
    }

    fn pattern_selector(&mut self, c: &mut Canvas, input: &Input, r: Rect) {
        let t = self.theme;
        c.fill_rrect(r, 10.0, t.raised);
        c.stroke_rrect(r, 10.0, 1.0, t.hairline);
        let n = self.project.patterns.len();
        let cur = self.project.current_pattern;
        let prev = Rect::new(r.x + 2.0, r.y + 2.0, 28.0, r.h - 4.0);
        let next = Rect::new(r.right() - 30.0, r.y + 2.0, 28.0, r.h - 4.0);
        if self.icon_button(c, input, wid("pat-prev", 0, 0), prev, Icon::ChevronLeft, false) && cur > 0 {
            self.select_pattern(cur - 1);
        }
        if self.icon_button(c, input, wid("pat-next", 0, 0), next, Icon::ChevronRight, false) {
            if cur + 1 < n {
                self.select_pattern(cur + 1);
            } else {
                let p = self.project.add_pattern();
                self.select_pattern(p);
                self.toast("Nouveau pattern");
            }
        }
        let mid = Rect::new(prev.right(), r.y, next.x - prev.right(), r.h);
        let id = wid("pat-name", 0, 0);
        let hov = self.hovered(input, id, mid);
        let hv = self.hover_anim(id, hov);
        if hov && (input.pressed || input.right_pressed) {
            self.open_popup(Popup::Patterns, r);
        }
        let name = self.project.patterns[cur].name.clone();
        let (cx, cy) = mid.center();
        let label = self.fit(self.f.sans_med, 13.0, &name, mid.w - 8.0);
        self.text_center(c, self.f.sans_med, 13.0, cx, cy, &label, t.text2.mix(t.text, 0.6 + 0.4 * hv));
    }

    fn position_field(&mut self, c: &mut Canvas, r: Rect) {
        let t = self.theme;
        c.fill_rrect(r, 10.0, t.raised);
        c.stroke_rrect(r, 10.0, 1.0, t.hairline);
        let (_, cy) = r.center();
        let pos = self.cycle_position();
        let beat_in_bar = ((pos % TICKS_PER_BAR as f64) / 96.0).floor() as usize;
        for b in 0..4 {
            let lit = self.playing && b == beat_in_bar;
            let v = self.anim.to(wid("beat-led", b, 0), lit as u8 as f32, T_HEAD);
            c.fill_circle(r.x + 14.0 + b as f32 * 11.0, cy, 3.0, t.raised_hi.mix(t.accent, v));
        }
        let bar = (pos / TICKS_PER_BAR as f64).floor() as u32 + 1;
        let beat = beat_in_bar + 1;
        let step = ((pos % 96.0) / TICKS_PER_STEP as f64).floor() as u32 + 1;
        let text = format!("{bar}.{beat}.{step}");
        self.text_right(c, self.f.mono_med, 15.0, r.right() - 12.0, cy, &text, if self.playing { t.text } else { t.text2 });
    }

    // --- Zone principale ---

    fn main_area(&mut self, c: &mut Canvas, input: &Input, r: Rect) {
        let t = self.theme;
        let tabs = Rect::new(r.x + PAD, r.y + 12.0, 580.0f32.min(r.w - 2.0 * PAD), 36.0);
        let labels: Vec<&str> = View::ALL.iter().map(|v| v.label()).collect();
        if let Some(i) = self.segmented(c, input, wid("tabs", 0, 0), tabs, &labels, self.view.index()) {
            self.set_view(View::ALL[i]);
        }
        let header = Rect::new(tabs.right() + 16.0, r.y + 12.0, r.right() - PAD - tabs.right() - 16.0, 36.0);
        let body = Rect::new(r.x + PAD, r.y + 60.0, r.w - 2.0 * PAD, r.h - 60.0 - PAD);
        if body.h < 40.0 || body.w < 100.0 {
            return;
        }

        // Entrée de vue : fondu et légère montée.
        let enter = self.anim.to(wid("view-enter", 0, 0), 1.0, T_LAYOUT);
        let rise = ((1.0 - enter) * 10.0).round();
        let body_shifted = body.translate(0.0, rise);
        let prev = c.push_clip(Rect::new(r.x, header.y, r.w, r.bottom() - header.y));
        match self.view {
            View::Rack => self.rack_view(c, input, header, body_shifted),
            View::Piano => self.piano_view(c, input, header, body_shifted),
            View::Playlist => self.playlist_view(c, input, header, body_shifted),
            View::Session => self.session_view(c, input, header, body_shifted),
            View::Mixer => self.mixer_view(c, input, header, body_shifted),
        }
        if enter < 0.999 {
            c.fill_rect(Rect::new(r.x, header.y - 2.0, r.w, r.bottom() - header.y + 2.0), t.bg.alpha(1.0 - enter));
        }
        c.set_clip(Some(prev));
    }

    // --- Panneau d'appareils ---

    fn panel_area(&mut self, c: &mut Canvas, input: &Input, r: Rect) {
        let t = self.theme;
        c.fill_rect(r, t.bg_deep);
        c.fill_rect(Rect::new(r.x, r.y, r.w, 1.0), t.hairline);
        let prev = c.push_clip(r);
        self.device_panel(c, input, Rect::new(r.x, r.y + 1.0, r.w, PANEL - 1.0));
        c.set_clip(Some(prev));
    }

    // --- Navigateur ---

    fn sidebar_area(&mut self, c: &mut Canvas, input: &Input, r: Rect) {
        let t = self.theme;
        c.fill_rect(r, t.bg_deep);
        c.fill_rect(Rect::new(r.right() - 1.0, r.y, 1.0, r.h), t.hairline);
        let prev = c.push_clip(r);
        let inner_x = r.right() - SIDEBAR + 12.0;
        let w = SIDEBAR - 24.0;

        // Thème, en bas, fixe.
        let theme_box = Rect::new(inner_x, r.bottom() - 52.0, w, 36.0);
        let names: Vec<&str> = THEMES.iter().map(|t| t.name).collect();
        let cur = THEMES.iter().position(|x| x.name == self.theme.name).unwrap_or(0);
        if let Some(i) = self.segmented(c, input, wid("theme", 0, 0), theme_box, &names, cur) {
            self.theme = THEMES[i];
        }
        c.fill_rect(Rect::new(r.x, theme_box.y - 12.0, r.w - 1.0, 1.0), t.hairline);

        // Recherche : filtre instruments, effets et échantillons.
        let field = Rect::new(inner_x, r.y + 12.0, w, 36.0);
        let fid = wid("sb-search", 0, 0);
        let fhov = self.hovered(input, fid, field);
        if fhov && input.pressed {
            self.sidebar_focus = true;
        } else if input.pressed && !field.contains(input.x, input.y) {
            self.sidebar_focus = false;
        }
        let mut enter = false;
        if self.sidebar_focus {
            self.sidebar_query.push_str(&input.text);
            for &(key, _) in &input.keys {
                match key {
                    Key::Backspace => {
                        self.sidebar_query.pop();
                    }
                    Key::Escape => {
                        self.sidebar_query.clear();
                        self.sidebar_focus = false;
                    }
                    Key::Enter => enter = true,
                    _ => {}
                }
            }
            if !input.text.is_empty() || !input.keys.is_empty() {
                self.sidebar_scroll = 0.0;
            }
        }
        let fv = self.anim.to(wid("sb-search-f", 0, 0), self.sidebar_focus as u8 as f32, T_HOVER);
        let fhv = self.hover_anim(fid, fhov);
        c.fill_rrect(field, 10.0, t.raised.mix(t.raised_hi, fhv * 0.5));
        c.stroke_rrect(field, 10.0, 1.0 + fv * 0.5, t.hairline.mix(t.select, fv * 0.8));
        c.stroke_circle(field.x + 16.0, field.y + 17.0, 5.0, 1.5, t.muted);
        c.stroke_polyline(&[(field.x + 20.0, field.y + 21.0), (field.x + 23.0, field.y + 24.0)], 1.5, t.muted);
        let query = self.sidebar_query.clone();
        let (shown, ink) = if query.is_empty() && !self.sidebar_focus { ("Rechercher  Ctrl F".to_string(), t.muted) } else { (query.clone(), t.text) };
        let tw = self.text(c, self.f.sans, 13.0, field.x + 32.0, field.y + 18.0, &shown, ink);
        if self.sidebar_focus {
            if (self.time * 2.0).fract() < 0.6 {
                c.fill_rect(Rect::new(field.x + 33.0 + if query.is_empty() { 0.0 } else { tw }, field.y + 9.0, 1.5, 18.0), t.select);
            }
            self.anim.set(wid("caret", 0, 0), self.time);
        }
        let list = Rect::new(r.x, field.bottom() + 8.0, r.w, theme_box.y - 12.0 - field.bottom() - 8.0);

        let hov_list = input.inside && list.contains(input.x, input.y);
        if hov_list && input.wheel != 0.0 {
            self.sidebar_scroll = (self.sidebar_scroll - input.wheel * 60.0).max(0.0);
        }
        let scroll = self.anim.to(wid("sb-scroll", 0, 0), self.sidebar_scroll, T_LAYOUT);
        let inner = c.push_clip(list);
        let mut y = list.y + 12.0 - scroll;

        let section = |app: &mut App, c: &mut Canvas, y: &mut f32, title: &str| {
            app.text(c, app.f.sans_med, 12.0, inner_x + 4.0, *y + 8.0, title, t.muted);
            *y += 24.0;
        };
        // Filtre et tri par pertinence.
        let rank = |items: Vec<(usize, String, String)>| -> Vec<(usize, String, String)> {
            if query.trim().is_empty() {
                return items;
            }
            let mut scored: Vec<(i32, (usize, String, String))> = items
                .into_iter()
                .filter_map(|it| super::palette::fuzzy_score(&query, &format!("{} {}", it.1, it.2)).map(|s| (s, it)))
                .collect();
            scored.sort_by_key(|s| std::cmp::Reverse(s.0));
            scored.into_iter().map(|(_, it)| it).collect()
        };
        let insts = rank(registry::instruments().iter().enumerate().map(|(i, k)| (i, k.name.to_string(), k.description.to_string())).collect());
        let fxs = rank(registry::effects().iter().enumerate().map(|(i, k)| (i, k.name.to_string(), k.description.to_string())).collect());
        let files = self.sample_files.clone();
        let wavs = rank(
            files
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    let name = p.file_stem().map_or(String::new(), |s| s.to_string_lossy().into_owned());
                    let folder = p.parent().and_then(|p| p.file_name()).map_or(String::new(), |s| s.to_string_lossy().into_owned());
                    (i, name, folder)
                })
                .collect(),
        );
        if enter {
            if let Some((i, _, _)) = insts.first() {
                self.add_instrument(registry::instruments()[*i].id);
            } else if let Some((i, _, _)) = fxs.first() {
                let track = self.focused_track();
                self.add_effect(track, registry::effects()[*i].id);
            } else if let Some((i, _, _)) = wavs.first() {
                self.add_sample_channel(&files[*i].to_string_lossy());
            }
        }

        if !insts.is_empty() {
            section(self, c, &mut y, "Instruments");
        }
        for (i, name, desc) in &insts {
            let row = Rect::new(inner_x, y, w, 40.0);
            if self.browser_row(c, input, wid("br-inst", *i, 0), row, name, desc) {
                self.add_instrument(registry::instruments()[*i].id);
            }
            y += 42.0;
        }
        if !fxs.is_empty() {
            y += 16.0;
            section(self, c, &mut y, "Effets");
        }
        for (i, name, desc) in &fxs {
            let row = Rect::new(inner_x, y, w, 40.0);
            if self.browser_row(c, input, wid("br-fx", *i, 0), row, name, desc) {
                let track = self.focused_track();
                self.add_effect(track, registry::effects()[*i].id);
            }
            y += 42.0;
        }
        y += 16.0;
        section(self, c, &mut y, "Échantillons");
        if wavs.is_empty() {
            let msg = if files.is_empty() { "Aucun fichier WAV trouvé" } else { "Aucun résultat" };
            self.text(c, self.f.sans, 12.0, inner_x + 4.0, y + 10.0, msg, t.muted);
            y += 28.0;
        }
        for (i, name, folder) in &wavs {
            let row = Rect::new(inner_x, y, w, 40.0);
            if self.browser_row(c, input, wid("br-wav", *i, 0), row, name, folder) {
                let path = files[*i].to_string_lossy().into_owned();
                if self.view == View::Playlist { self.add_audio_clip(&path) } else { self.add_sample_channel(&path) }
            }
            y += 42.0;
        }
        let content_h = y + scroll - list.y + 16.0;
        self.sidebar_scroll = self.sidebar_scroll.min((content_h - list.h).max(0.0));
        c.set_clip(Some(inner));
        c.set_clip(Some(prev));
    }

    fn browser_row(&mut self, c: &mut Canvas, input: &Input, id: u64, row: Rect, title: &str, detail: &str) -> bool {
        let t = self.theme;
        let hov = self.hovered(input, id, row);
        let hv = self.hover_anim(id, hov);
        if hv > 0.01 {
            c.fill_rrect(row, 8.0, t.raised.alpha(hv));
        }
        self.text_fit(c, self.f.sans_med, 13.0, row.x + 10.0, row.y + 13.0, title, row.w - 40.0, t.text);
        self.text_fit(c, self.f.sans, 11.0, row.x + 10.0, row.y + 28.0, detail, row.w - 40.0, t.muted);
        if hv > 0.01 {
            draw_icon(c, Icon::Plus, row.right() - 16.0, row.y + row.h * 0.5, 14.0, t.accent.alpha(hv));
        }
        hov && input.pressed
    }

    // --- Menus ---

    pub fn open_popup(&mut self, kind: Popup, anchor: Rect) {
        self.popup = Some(PopupState { kind, anchor });
        self.anim.set(wid("popup", 0, 0), 0.0);
        self.anim.set(wid("popup-scroll", 0, 0), 0.0);
    }

    /// Lignes d'un menu : (titre, détail, action).
    fn menu_items(&self, kind: &Popup) -> (String, Vec<(String, String, Action)>) {
        let p = &self.project;
        let track_name = |t: usize| p.tracks[t].name.clone();
        match kind {
            Popup::AddInstrument { replace } => (
                if replace.is_some() { "Remplacer l'instrument" } else { "Ajouter un instrument" }.into(),
                registry::instruments()
                    .iter()
                    .map(|k| (k.name.to_string(), k.description.to_string(), Action::AddInstrument(k.id, *replace)))
                    .collect(),
            ),
            Popup::AddEffect { track } => (
                format!("Effet sur {}", track_name(*track)),
                registry::effects()
                    .iter()
                    .map(|k| (k.name.to_string(), k.category.to_string(), Action::AddEffect(*track, k.id)))
                    .collect(),
            ),
            Popup::Patterns => {
                let mut v: Vec<(String, String, Action)> = p
                    .patterns
                    .iter()
                    .enumerate()
                    .map(|(i, pat)| {
                        (pat.name.clone(), format!("{} mesure(s)", pat.len.div_ceil(TICKS_PER_BAR)), Action::SelectPattern(i))
                    })
                    .collect();
                v.push(("Nouveau pattern".into(), String::new(), Action::NewPattern));
                v.push(("Dupliquer ce pattern".into(), String::new(), Action::DuplicatePattern));
                let cur = p.current_pattern;
                v.push((
                    "Renommer ce pattern".into(),
                    String::new(),
                    Action::Popup(Popup::Rename { target: Rename::Pattern(cur), text: p.patterns[cur].name.clone() }),
                ));
                if p.patterns.len() > 1 {
                    v.push(("Supprimer ce pattern".into(), "Retire aussi ses clips".into(), Action::DeletePattern));
                }
                ("Patterns".into(), v)
            }
            Popup::OpenProject(files) => {
                let items = if files.is_empty() {
                    vec![("Aucun projet enregistré".into(), compat::projects_dir().display().to_string(), Action::None)]
                } else {
                    files
                        .iter()
                        .map(|f| {
                            (
                                f.file_stem().map_or(String::new(), |s| s.to_string_lossy().into_owned()),
                                f.parent().map_or(String::new(), |d| d.display().to_string()),
                                Action::Open(f.clone()),
                            )
                        })
                        .collect()
                };
                ("Ouvrir un projet".into(), items)
            }
            Popup::ChannelMenu(ch) => {
                let ch = *ch;
                let c = &p.channels[ch];
                (
                    c.name.clone(),
                    vec![
                        ("Ouvrir dans le piano roll".into(), String::new(), Action::ChannelPianoRoll(ch)),
                        (
                            "Renommer".into(),
                            String::new(),
                            Action::Popup(Popup::Rename { target: Rename::Channel(ch), text: c.name.clone() }),
                        ),
                        ("Remplacer l'instrument…".into(), String::new(), Action::Popup(Popup::AddInstrument { replace: Some(ch) })),
                        ("Dupliquer".into(), "Mêmes réglages, sans les notes".into(), Action::DuplicateChannel(ch)),
                        ("Supprimer".into(), "Retire aussi ses notes".into(), Action::DeleteChannel(ch)),
                    ],
                )
            }
            Popup::TrackMenu(t) => {
                let t = *t;
                let mut v = vec![(
                    "Renommer".into(),
                    String::new(),
                    Action::Popup(Popup::Rename { target: Rename::Track(t), text: p.tracks[t].name.clone() }),
                )];
                if t != MASTER {
                    v.push(("Sortie vers…".into(), format!("Actuellement : {}", track_name(p.tracks[t].output)), Action::Popup(Popup::Output(t))));
                    v.push(("Ajouter un envoi…".into(), String::new(), Action::Popup(Popup::Send(t))));
                }
                v.push(("Ajouter un effet…".into(), String::new(), Action::Popup(Popup::AddEffect { track: t })));
                v.push(("Réinitialiser volume et pan".into(), String::new(), Action::ResetTrack(t)));
                (p.tracks[t].name.clone(), v)
            }
            Popup::Output(t) | Popup::Send(t) => {
                let t = *t;
                let send = matches!(kind, Popup::Send(_));
                let v = (0..TRACKS)
                    .filter(|&d| d != t && self.routing_ok(t, d, send))
                    .filter(|&d| d == MASTER || self.track_in_use(d))
                    .map(|d| {
                        let action = if send { Action::AddSend(t, d) } else { Action::SetOutput(t, d) };
                        (track_name(d), if d == MASTER { "Master".into() } else { format!("Piste {d}") }, action)
                    })
                    .collect();
                (if send { "Envoyer vers" } else { "Sortie vers" }.into(), v)
            }
            Popup::Param { target, value, default } => {
                let label = self.target_info(*target).map_or(String::new(), |(l, _)| l);
                let mut v = vec![("Automatiser dans la playlist".into(), "Une courbe sur toute la durée du morceau".into(), Action::Automate(*target, *value))];
                if self.is_automated(*target) {
                    v.push(("Supprimer l'automation".into(), String::new(), Action::RemoveAutomation(*target)));
                }
                v.push(("Valeur par défaut".into(), String::new(), Action::SetParamValue(*target, *default)));
                (label, v)
            }
            Popup::SessionAssign { col, row } => (
                format!("Piste {} · scène {}", col + 1, row + 1),
                p.patterns
                    .iter()
                    .enumerate()
                    .map(|(i, pat)| (pat.name.clone(), format!("{} mesure(s)", pat.len.div_ceil(TICKS_PER_BAR)), Action::AssignSession(*col, *row, i)))
                    .collect(),
            ),
            Popup::Rename { .. } | Popup::Palette { .. } => (String::new(), Vec::new()),
        }
    }

    pub fn track_in_use(&self, t: usize) -> bool {
        self.project.channels.iter().any(|c| c.track == t)
            || self.project.tracks[t].inserts.iter().any(Option::is_some)
            || !self.project.tracks[t].name.starts_with("Piste ")
    }

    /// Vrai si router `from` vers `to` (sortie ou envoi) ne crée pas de boucle.
    fn routing_ok(&self, from: usize, to: usize, send: bool) -> bool {
        let mut p = self.project.clone();
        if send {
            if let Some(slot) = p.tracks[from].sends.iter().position(|s| s.1 <= 0.0) {
                p.tracks[from].sends[slot] = (to, 0.5);
            }
        } else {
            p.tracks[from].output = to;
        }
        p.routing_order().is_some()
    }

    fn run_action(&mut self, action: Action, anchor: Rect) {
        match action {
            Action::None => {}
            Action::AddInstrument(kind, None) => self.add_instrument(kind),
            Action::AddInstrument(kind, Some(ch)) => {
                if let Some(info) = registry::instrument_kind(kind) {
                    let c = &mut self.project.channels[ch];
                    c.instrument = InstrumentDef::new(kind);
                    if c.name.is_empty() || registry::instruments().iter().any(|k| k.name == c.name) {
                        c.name = info.name.to_string();
                    }
                    self.commit("Remplacer l'instrument");
                }
            }
            Action::AddEffect(track, kind) => self.add_effect(track, kind),
            Action::SelectPattern(i) => self.select_pattern(i),
            Action::NewPattern => {
                let p = self.project.add_pattern();
                self.select_pattern(p);
            }
            Action::DuplicatePattern => {
                let mut copy = self.project.patterns[self.project.current_pattern].clone();
                copy.name = format!("{} (copie)", copy.name);
                self.project.patterns.push(copy);
                let p = self.project.patterns.len() - 1;
                self.select_pattern(p);
            }
            Action::DeletePattern => self.delete_pattern(self.project.current_pattern),
            Action::Open(path) => self.open_file(path),
            Action::Popup(kind) => self.open_popup(kind, anchor),
            Action::DuplicateChannel(ch) => {
                let src = self.project.channels[ch].clone();
                let idx = self.project.add_channel(format!("{} (copie)", src.name), src.instrument.clone(), src.root_note);
                let c = &mut self.project.channels[idx];
                c.gain = src.gain;
                c.pan = src.pan;
                self.focus = Focus::Channel(idx);
                self.commit("Dupliquer un canal");
            }
            Action::DeleteChannel(ch) => {
                let name = self.project.channels[ch].name.clone();
                self.project.remove_channel(ch);
                self.clamp_selection();
                self.commit("Supprimer un canal");
                self.toast(&format!("{name} supprimé · Ctrl+Z pour annuler"));
            }
            Action::ChannelPianoRoll(ch) => {
                self.focus = Focus::Channel(ch);
                self.set_view(View::Piano);
            }
            Action::SetOutput(t, d) => {
                self.project.tracks[t].output = d;
                self.commit("Sortie de piste");
            }
            Action::AddSend(t, d) => {
                let sends = &mut self.project.tracks[t].sends;
                match sends.iter().position(|s| s.0 == d && s.1 > 0.0).or_else(|| sends.iter().position(|s| s.1 <= 0.0)) {
                    Some(slot) => {
                        sends[slot] = (d, 0.5);
                        self.commit("Ajouter un envoi");
                    }
                    None => self.toast("Plus d'envoi libre sur cette piste"),
                }
            }
            Action::ResetTrack(t) => {
                self.project.tracks[t].gain = 1.0;
                self.project.tracks[t].pan = 0.0;
                self.commit("Réinitialiser une piste");
            }
            Action::PlayStop => self.play_stop(),
            Action::ToggleMode => {
                let mode = match self.project.mode {
                    PlayMode::Pattern(_) => PlayMode::Song,
                    PlayMode::Song | PlayMode::Session => PlayMode::Pattern(self.project.current_pattern),
                };
                self.set_mode(mode);
            }
            Action::ToggleMetronome => {
                self.metronome = !self.metronome;
                let m = self.metronome;
                self.send(Command::SetMetronome(m));
            }
            Action::ToggleRecord => self.record = !self.record,
            Action::ToggleTyping => self.toggle_typing_keyboard(),
            Action::SetView(v) => self.set_view(v),
            Action::ToggleSidebar => self.sidebar = !self.sidebar,
            Action::TogglePanel => self.panel = !self.panel,
            Action::FocusChannel(ch) => {
                self.focus = Focus::Channel(ch);
                self.panel = true;
            }
            Action::Save => self.save(),
            Action::OpenMenu => self.open_popup(Popup::OpenProject(compat::list_projects()), anchor),
            Action::NewProject => {
                self.replace_project(Project::empty(), None);
                self.toast("Nouveau projet");
            }
            Action::Export => self.export(),
            Action::Undo => self.undo(),
            Action::Redo => self.redo(),
            Action::LoadDemo(i) => {
                let p = if i == 1 { ponzik_project::demo_breakcore() } else { ponzik_project::demo_project() };
                let name = p.name.clone();
                self.replace_project(p, None);
                self.toast(&format!("Démo chargée : {name}"));
            }
            Action::SetTheme(i) => self.theme = THEMES[i.min(THEMES.len() - 1)],
            Action::Automate(target, value) => self.create_automation(target, value),
            Action::AssignSession(col, row, p) => {
                self.project.session_grid[col][row] = Some(p);
                self.commit("Remplir une case");
            }
            Action::RemoveAutomation(target) => {
                self.project.automation.retain(|a| a.target != target);
                self.commit("Retirer une automation");
            }
            Action::SetParamValue(target, v) => {
                use ponzik_engine::AutoTarget as T;
                match target {
                    T::InstrumentParam { channel, id } => {
                        if let Some(c) = self.project.channels.get_mut(channel) {
                            c.instrument.set(id, v);
                        }
                    }
                    T::EffectParam { track, slot, id } => {
                        if let Some(Some(e)) = self.project.tracks.get_mut(track).map(|t| &mut t.inserts[slot]) {
                            e.set(id, v);
                        }
                    }
                    _ => {}
                }
                self.commit("Valeur par défaut");
            }
        }
    }

    fn delete_pattern(&mut self, p: usize) {
        if self.project.patterns.len() <= 1 {
            return;
        }
        self.project.patterns.remove(p);
        self.project.clips.retain(|c| c.pattern != p);
        for c in &mut self.project.clips {
            if c.pattern > p {
                c.pattern -= 1;
            }
        }
        let n = self.project.patterns.len();
        if let PlayMode::Pattern(x) = self.project.mode {
            self.project.mode = PlayMode::Pattern(if x > p { x - 1 } else { x.min(n - 1) });
        }
        self.project.current_pattern = self.project.current_pattern.min(n - 1);
        self.playlist.brush = self.playlist.brush.min(n - 1);
        self.commit("Supprimer un pattern");
    }

    fn apply_rename(&mut self, target: Rename, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        match target {
            Rename::Project => self.project.name = text.into(),
            Rename::Channel(c) => {
                let old = std::mem::replace(&mut self.project.channels[c].name, text.into());
                let t = self.project.channels[c].track;
                if self.project.tracks[t].name == old {
                    self.project.tracks[t].name = text.into();
                }
            }
            Rename::Pattern(p) => self.project.patterns[p].name = text.into(),
            Rename::Track(t) => self.project.tracks[t].name = text.into(),
        }
        self.commit("Renommer");
    }

    fn popup_layer(&mut self, c: &mut Canvas, input: &Input, screen: Rect) {
        let Some(mut state) = self.popup.take() else { return };
        let t = self.theme;
        if let Popup::Palette { query, selected } = &state.kind {
            let (run, next) = self.palette(c, input, screen, query.clone(), *selected);
            if let Some(kind) = next {
                self.popup = Some(PopupState { kind, anchor: state.anchor });
            }
            if let Some(action) = run {
                self.run_action(action, Rect::new(screen.w * 0.5 - 150.0, screen.h * 0.2, 300.0, 0.0));
            }
            return;
        }
        let open = self.anim.to(wid("popup", 0, 0), 1.0, T_HOVER);

        // Fenêtre de renommage : un champ texte.
        if let Popup::Rename { target, text } = &mut state.kind {
            text.push_str(&input.text);
            let mut done = None;
            for &(key, _) in &input.keys {
                match key {
                    Key::Backspace => {
                        text.pop();
                    }
                    Key::Enter => done = Some(true),
                    Key::Escape => done = Some(false),
                    _ => {}
                }
            }
            let w = 360.0;
            let r = Rect::new((screen.w - w) * 0.5, screen.h * 0.32 + (1.0 - open) * 8.0, w, 132.0);
            c.fill_rect(screen, Color::rgba(0, 0, 0, (110.0 * open) as u8));
            c.fill_rrect(r, 16.0, t.panel);
            c.stroke_rrect(r, 16.0, 1.0, t.hairline.alpha(1.6));
            self.text(c, self.f.sans_med, 14.0, r.x + 20.0, r.y + 26.0, "Renommer", t.text);
            let field = Rect::new(r.x + 16.0, r.y + 46.0, r.w - 32.0, 40.0);
            c.fill_rrect(field, 10.0, t.well);
            c.stroke_rrect(field, 10.0, 1.5, t.select.alpha(0.8));
            let shown = self.fit(self.f.sans, 15.0, text, field.w - 30.0);
            let tw = self.text(c, self.f.sans, 15.0, field.x + 12.0, field.y + 20.0, &shown, t.text);
            if (self.time * 2.0).fract() < 0.6 {
                c.fill_rect(Rect::new(field.x + 13.0 + tw, field.y + 10.0, 1.5, 20.0), t.select);
            }
            // Tient l'animation éveillée pour faire clignoter le curseur.
            self.anim.set(wid("caret", 0, 0), self.time);
            self.text(c, self.f.sans, 12.0, r.x + 20.0, r.y + 108.0, "Entrée pour valider · Échap pour annuler", t.muted);
            if input.pressed && !r.contains(input.x, input.y) {
                done = Some(false);
            }
            match done {
                Some(true) => {
                    let (target, text) = (*target, text.clone());
                    self.apply_rename(target, &text);
                }
                Some(false) => {}
                None => self.popup = Some(state),
            }
            return;
        }

        let (title, items) = self.menu_items(&state.kind);
        let w = 300.0;
        let row_h = 44.0;
        let h = (52.0 + items.len() as f32 * row_h + 8.0).min(screen.h - 80.0);
        let mut x = state.anchor.x;
        let mut y = state.anchor.bottom() + 8.0;
        if x + w > screen.right() - 12.0 {
            x = screen.right() - 12.0 - w;
        }
        if y + h > screen.bottom() - 12.0 {
            y = (state.anchor.y - 8.0 - h).max(TOP_BAR);
        }
        let r = Rect::new(x.max(12.0), y + (1.0 - open) * 6.0, w, h);
        c.fill_rrect(r.translate(0.0, 6.0).inset(-2.0), 16.0, Color::rgba(0, 0, 0, (90.0 * open) as u8));
        c.fill_rrect(r, 14.0, t.panel);
        c.stroke_rrect(r, 14.0, 1.0, t.hairline.alpha(1.6));
        self.text_fit(c, self.f.sans_med, 13.0, r.x + 16.0, r.y + 24.0, &title, r.w - 32.0, t.text2);

        // Défilement si le menu est plus long que l'écran.
        let list_h = r.h - 52.0;
        let content = items.len() as f32 * row_h;
        let key = wid("popup-scroll", 0, 0);
        let mut scroll = self.anim.get(key).unwrap_or(0.0);
        if input.inside && r.contains(input.x, input.y) && input.wheel != 0.0 {
            scroll -= input.wheel * 60.0;
        }
        scroll = scroll.clamp(0.0, (content - list_h).max(0.0));
        self.anim.set(key, scroll);

        let mut chosen = None;
        let prev = c.push_clip(Rect::new(r.x, r.y + 44.0, r.w, r.h - 48.0));
        for (i, (name, detail, _)) in items.iter().enumerate() {
            let row = Rect::new(r.x + 6.0, r.y + 44.0 + i as f32 * row_h - scroll, r.w - 12.0, row_h - 4.0);
            let id = wid("popup-row", i, 0);
            let hov = input.inside && row.contains(input.x, input.y) && r.contains(input.x, input.y);
            let hv = self.hover_anim(id, hov);
            let current = matches!(state.kind, Popup::Patterns) && i == self.project.current_pattern;
            if hv > 0.01 || current {
                c.fill_rrect(row, 10.0, t.raised.mix(t.raised_hi, hv * 0.6).alpha(hv.max(current as u8 as f32 * 0.7)));
            }
            if current {
                c.fill_rrect(Rect::new(row.x + 4.0, row.y + 12.0, 3.0, row.h - 24.0), 1.5, t.accent);
            }
            let cy = if detail.is_empty() { row.y + row.h * 0.5 } else { row.y + 13.0 };
            let danger = name.starts_with("Supprimer");
            self.text_fit(c, self.f.sans_med, 13.0, row.x + 14.0, cy, name, row.w - 28.0, if danger { t.negative } else { t.text });
            if !detail.is_empty() {
                self.text_fit(c, self.f.sans, 11.0, row.x + 14.0, row.y + 28.0, detail, row.w - 28.0, t.muted);
            }
            if hov && input.pressed {
                chosen = Some((i, row));
            }
        }
        c.set_clip(Some(prev));

        let clicked_outside = (input.pressed || input.right_pressed) && !r.contains(input.x, input.y);
        if let Some((i, row)) = chosen {
            self.anim.set(key, 0.0);
            let action = items[i].2.clone();
            self.run_action(action, Rect::new(r.x, row.y, r.w, row.h));
        } else if !clicked_outside {
            self.popup = Some(state);
        }
    }

    fn toast_layer(&mut self, c: &mut Canvas, dt: f32, area: Rect) {
        let t = self.theme;
        let mut y = area.bottom() - 16.0;
        let mut i = self.toasts.len();
        while i > 0 {
            i -= 1;
            self.toasts[i].age += dt;
            let age = self.toasts[i].age;
            let alpha = (age / 0.18).min(1.0) * ((3.2 - age) / 0.3).clamp(0.0, 1.0);
            if alpha <= 0.0 && age > 3.0 {
                self.toasts.remove(i);
                continue;
            }
            let text = self.toasts[i].text.clone();
            let tw = self.fonts.measure(self.f.sans, 13.0, &text).min(area.w - 80.0);
            let r = Rect::new(area.right() - 16.0 - tw - 40.0, y - 40.0 + (1.0 - alpha) * 8.0, tw + 40.0, 40.0);
            c.fill_rrect(r, 12.0, t.raised_hi.alpha(alpha));
            c.stroke_rrect(r, 12.0, 1.0, t.hairline.alpha(alpha));
            c.fill_circle(r.x + 16.0, r.y + 20.0, 3.0, t.accent.alpha(alpha));
            self.text_fit(c, self.f.sans, 13.0, r.x + 28.0, r.y + 20.0, &text, tw, t.text.alpha(alpha));
            y -= 48.0;
        }
    }
}
