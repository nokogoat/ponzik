//! État de l'interface, navigation, barre de transport, navigateur, menus.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use ponzik_dsp::loudness::LoudnessMeter;
use ponzik_dsp::spectrum::SpectrumAnalyzer;
use ponzik_engine::mixer::{MASTER, TRACKS};
use ponzik_engine::registry;
use ponzik_engine::{Command, EngineHandle, PlayMode, Sample, TICKS_PER_BAR, TICKS_PER_STEP};
use ponzik_i18n::{tr, tr_ctx, tr_fmt, tr_noop};
use ponzik_project::autosave::{self, Autosaver};
use ponzik_project::{ChordKind, EffectDef, InstrumentDef, Project, ScaleKind, TrackKind};
use ponzik_ui::{Anim, Canvas, Color, FontId, Fonts, Rect, embedded};
use ponzik_window::Key;

use super::Input;
use super::compat;
use super::dnd::Payload;

/// Entrée du navigateur : (catégorie, nom, description, contenu).
type BrowserEntry = (usize, String, String, Payload);
/// Ce dont dépendent les entrées : échantillons (adresse, nombre), plugins, langue.
type BrowserKey = (usize, usize, usize, &'static str);
use super::theme::*;
use super::widgets::{Icon, TipState, draw_icon, wid};

const SCOPE_LEN: usize = 8_192;
/// Le moteur ne garde qu'un échantillon sur deux du master pour l'interface.
const SCOPE_DECIMATION: u32 = 2;
/// Taille de l'analyseur de spectre du master.
const SPECTRUM_SIZE: usize = 4_096;
/// Échantillons par paquet de crêtes des formes d'onde.
pub const WAVE_BUCKET: usize = 256;
const HISTORY_CAP: usize = 200;
/// Mémoire au-delà de laquelle les plus vieilles étapes d'annulation sont
/// oubliées : un gros projet garde moins d'étapes qu'un petit.
const HISTORY_BYTES: usize = 32 << 20;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum View {
    Rack,
    Piano,
    Playlist,
    Session,
    Mixer,
    /// Atelier : éditeur d'échantillons.
    Lab,
}

impl View {
    pub const ALL: [View; 6] = [View::Rack, View::Piano, View::Playlist, View::Session, View::Mixer, View::Lab];

    pub fn icon(self) -> Icon {
        match self {
            View::Rack => Icon::Rack,
            View::Piano => Icon::Piano,
            View::Playlist => Icon::Playlist,
            View::Session => Icon::Session,
            View::Mixer => Icon::Mixer,
            View::Lab => Icon::Lab,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            View::Rack => tr("Channel Rack"),
            View::Piano => tr("Piano roll"),
            View::Playlist => tr("Playlist"),
            View::Session => tr("Session"),
            View::Mixer => tr("Mixer"),
            View::Lab => tr("Sample Lab"),
        }
    }

    pub fn index(self) -> usize {
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
    /// Choix d'un effet de notes pour un emplacement d'un canal.
    AddNoteFx { channel: usize, slot: usize },
    Patterns,
    OpenProject(Vec<PathBuf>),
    ChannelMenu(usize),
    TrackMenu(usize),
    /// Destination de la sortie d'une piste.
    Output(usize),
    /// Destination d'un nouvel envoi.
    Send(usize),
    /// Piste clé (entrée latérale) d'un insert.
    Sidechain { track: usize, slot: usize },
    /// Nouveau bus, retour d'effet ou VCA (bouton du mixer).
    NewBus,
    /// Bus de retour d'effet nourri par un envoi de la piste.
    NewReturn(usize),
    /// VCA qui pilote la piste.
    Vca(usize),
    Rename { target: Rename, text: String },
    /// Palette touche-à-tout (Ctrl+K).
    Palette { query: String, selected: usize },
    /// Fichiers MIDI à importer.
    ImportMidi(Vec<PathBuf>),
    /// FL Studio projects to import.
    ImportFlp(Vec<PathBuf>),
    /// Report of the last FL Studio import.
    ImportReport,
    /// Presets d'un appareil : enregistrer, ou charger un preset existant.
    Presets { device: super::devices::Device, kind: String },
    /// Choix du pattern d'une case vide de la vue Session.
    SessionAssign { col: usize, row: usize },
    /// Menu d'un paramètre (clic droit sur un potard).
    Param { target: ponzik_engine::AutoTarget, value: f32, default: f32 },
    /// Nouveau projet : choix d'un modèle.
    Templates,
    /// Tonalité et gamme du projet.
    Scale,
    /// Type d'accord posé par le tampon du piano roll.
    Chord,
    /// Force de l'humanisation des notes.
    Humanize,
    /// Historique des modifications.
    History,
    /// Menu principal : fichier, export, démos, thème.
    MainMenu,
    /// Choix du thème.
    Themes,
    /// Clic droit sur un clip de pattern de la playlist.
    ClipMenu(usize),
    /// Clic droit sur un pattern de la liste de la playlist.
    PatternMenu(usize),
    /// Couleur d'un pattern.
    PatternColor(usize),
    /// Réglages : espace de travail (0) ou raccourcis (1).
    Settings { tab: usize },
    /// Right-click on a Drum Pads pad.
    PadMenu { channel: usize, pad: usize },
    /// Right-click on an arrangement section.
    MarkerMenu(usize),
    MarkerColor(usize),
    /// Time signature from this tick (tempo lane).
    Signature(u32),
    /// New modulator: on the channel's instrument (if any) or the track.
    AddModulator { track: usize, channel: Option<usize> },
    /// An effect for one chain of a rack.
    RackAdd { track: usize, slot: usize, chain: usize },
    /// Right-click on a device inside a rack.
    RackDevice { track: usize, slot: usize, unit: usize },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rename {
    /// Nom d'un nouveau preset de l'appareil.
    Preset(super::devices::Device),
    Project,
    /// Nouveau nom puis enregistrement dans un nouveau fichier.
    SaveAs,
    Channel(usize),
    Pattern(usize),
    Track(usize),
    Marker(usize),
}

/// Format badge of a plugin device kind (`CLAP`, `VST3`), none for native devices.
pub fn plugin_badge(kind: &str) -> Option<&'static str> {
    if kind.starts_with(ponzik_clap::KIND_PREFIX) {
        Some("CLAP")
    } else if kind.starts_with(ponzik_vst3::KIND_PREFIX) {
        Some("VST3")
    } else {
        None
    }
}

/// Nom lisible d'un type d'appareil hors registre (plugin CLAP : son
/// identifiant ; plugin VST 3 : le nom de son paquet).
pub fn display_kind(kind: &str) -> String {
    if let Some((_, id)) = ponzik_clap::parse_kind(kind) {
        return id.rsplit('.').next().unwrap_or(id).to_string();
    }
    match ponzik_vst3::parse_kind(kind) {
        Some((path, id)) => path.file_stem().map_or_else(|| id.to_string(), |s| s.to_string_lossy().into_owned()),
        None => kind.to_string(),
    }
}

/// Ce que fait une ligne de menu ou de palette.
#[derive(Clone, Debug)]
pub enum Action {
    None,
    StartTour,
    /// Edit of an effect rack.
    Rack(super::rackfx::RackAction),
    /// Edit of the modulators.
    Mod(super::modulators::ModAction),
    AddInstrument(&'static str, Option<usize>),
    AddEffect(usize, &'static str),
    /// Effet de notes : canal, emplacement, type.
    AddNoteFx(usize, usize, &'static str),
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
    /// Piste clé d'un insert (piste, emplacement), ou aucune.
    SetSidechain(usize, usize, Option<usize>),
    ResetTrack(usize),
    PlayStop,
    ToggleMode,
    ToggleMetronome,
    ToggleRecord,
    /// Pré-compte d'une mesure avant l'enregistrement.
    ToggleCountIn,
    /// Horloge MIDI sur un port de sortie.
    ToggleMidiClock(usize),
    /// Un appui de tap tempo.
    TapTempo,
    ToggleTyping,
    SetView(View),
    ToggleSidebar,
    TogglePanel,
    FocusChannel(usize),
    Save,
    OpenMenu,
    NewProject,
    /// Nouveau projet à partir du modèle n.
    NewFromTemplate(usize),
    Export,
    /// Export WAV avec un format et une normalisation.
    ExportWith(ponzik_project::ExportOptions),
    Undo,
    Redo,
    LoadDemo(usize),
    SetTheme(usize),
    Automate(ponzik_engine::AutoTarget, f32),
    SaveAs,
    ExportStems,
    /// One WAV per arrangement section.
    ExportSections(ponzik_project::ExportOptions),
    Marker(usize, super::arrange::MarkerOp),
    /// Adds a marker at the playhead.
    AddMarkerHere,
    SetSignature(u32, u8, u8),
    DeleteSignature(u32),
    LoadPreset(super::devices::Device, PathBuf),
    /// A factory preset of the device, by name.
    LoadFactoryPreset(super::devices::Device, &'static str),
    ImportMidiMenu,
    ImportMidi(PathBuf),
    ImportFlpMenu,
    ImportFlp(PathBuf),
    ExportMidi,
    AssignSession(usize, usize, usize),
    RemoveAutomation(ponzik_engine::AutoTarget),
    /// Humanise les notes du piano roll : écart de début (ticks), écart relatif de vélocité.
    Humanize(u32, f32),
    /// Revient à l'état qui suit les n premières étapes de l'historique.
    JumpHistory(usize),
    SetParamValue(ponzik_engine::AutoTarget, f32),
    /// Le clip reçoit sa propre copie du pattern.
    MakeUnique(usize),
    /// Couleur d'un pattern (0 : celle de sa place dans la liste).
    SetPatternColor(usize, u32),
    /// Ouvre un pattern dans le Channel Rack.
    EditPattern(usize),
    DeleteClip(usize),
    /// Recopie la sélection de temps de la playlist juste après elle.
    DuplicateTime,
    /// Nouveau bus dans lequel sortent ces pistes (aucune : bus vide).
    NewBus(Vec<usize>),
    /// Bus de retour (type d'effet), avec un envoi depuis la piste donnée.
    NewReturn(&'static str, Option<usize>),
    /// Nouveau VCA qui pilote ces pistes.
    NewVca(Vec<usize>),
    /// VCA d'une piste, ou aucun.
    SetVca(usize, Option<usize>),
    /// Replie ou déplie un bus.
    ToggleFold(usize),
    /// Défait un bus ou un VCA.
    Dissolve(usize),
    /// Profil d'espace de travail n.
    ApplyPreset(usize),
    /// Langue de l'interface (`None` : celle du système).
    SetLanguage(Option<ponzik_i18n::Lang>),
    /// Affiche ou masque une vue.
    ToggleViewShown(View),
    /// Edit of a Drum Pads pad: channel, pad, operation.
    Pad(usize, usize, super::pads::PadOp),
    /// Scans every plugin again, cache ignored.
    RescanPlugins,
    /// Opens or closes a plugin device's own window.
    ToggleEditor(super::devices::Device),
}

/// Ligne de menu : une icône (ou la couleur d'un pattern), un nom court, une
/// valeur à droite ; la description passe en infobulle.
pub struct MenuItem {
    pub icon: Option<Icon>,
    pub color: Option<Color>,
    pub name: String,
    pub detail: String,
    pub tip: String,
    /// Filet de séparation au-dessus.
    pub sep: bool,
    /// Choix actuel, mis en évidence.
    pub on: bool,
    /// Ligne en retrait (étape annulée de l'historique, par exemple).
    pub dim: bool,
    pub action: Action,
}

impl MenuItem {
    pub(super) fn plain(name: &str, action: Action) -> Self {
        Self { icon: None, color: None, name: name.into(), detail: String::new(), tip: String::new(), sep: false, on: false, dim: false, action }
    }

    pub(super) fn new(icon: Icon, name: &str, action: Action) -> Self {
        Self { icon: Some(icon), ..Self::plain(name, action) }
    }

    pub(super) fn detail(mut self, d: String) -> Self {
        self.detail = d;
        self
    }

    pub(super) fn tip(mut self, t: &str) -> Self {
        self.tip = t.into();
        self
    }

    pub(super) fn sep(mut self) -> Self {
        self.sep = true;
        self
    }

    pub(super) fn on(mut self, on: bool) -> Self {
        self.on = on;
        self
    }

    fn dim(mut self, dim: bool) -> Self {
        self.dim = dim;
        self
    }
}

pub struct PopupState {
    pub kind: Popup,
    pub anchor: Rect,
    /// Ouvert pendant cette image : le clic qui l'a ouvert ne doit pas le refermer.
    pub fresh: bool,
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
    /// Les notes posées ou déplacées collent à la gamme du projet.
    pub scale_snap: bool,
    /// Un clic pose un accord entier : de ce type, ou celui de la gamme (`None`).
    pub chord_stamp: bool,
    pub chord: Option<ChordKind>,
    /// Renversement de l'accord posé (0 = fondamental).
    pub chord_inv: u8,
    pub voicing: ponzik_project::Voicing,
    /// Notes effacées à l'instant (note, heure) : elles s'évanouissent.
    pub erased: Vec<(ponzik_engine::Note, f32)>,
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
    pub(super) synced: Project,
    past: Vec<(String, Project)>,
    future: Vec<(String, Project)>,
    last_edit: Option<(String, f32)>,
    pub modified: bool,
    pub project_path: Option<PathBuf>,
    /// Rows of the last FL Studio import report (text, continuation of the previous row).
    pub import_report: Vec<(String, bool)>,

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
    /// Tranches du mixer choisies par Ctrl+clic (à grouper dans un bus ou un VCA).
    pub mixer_sel: Vec<usize>,
    pub samples: HashMap<String, Arc<Sample>>,
    /// Crêtes (min, max) par paquets d'échantillons, pour dessiner les formes d'onde.
    pub waveforms: HashMap<String, Arc<Vec<(f32, f32)>>>,
    /// Réponses d'effets déjà calculées, par réglages.
    pub responses: HashMap<String, Arc<Vec<f32>>>,
    pub sample_files: Vec<PathBuf>,
    /// Sound library being built in the background.
    pub library_build: Option<std::sync::mpsc::Receiver<Result<usize, String>>>,
    /// CLAP plugins found by the scan, by name.
    pub plugins: Vec<ponzik_clap::PluginInfo>,
    /// Dropouts of the audio output (see `dropouts.rs`).
    pub xrun_watch: super::dropouts::XrunWatch,
    /// VST 3 plugins found by the scan, by name.
    pub vst3_plugins: Vec<ponzik_vst3::PluginInfo>,
    /// Plugin files the scan could not load, shown in the browser.
    pub scan_errors: Vec<ponzik_project::plugins::ScanError>,
    /// Background scan in progress.
    pub plugin_scan: Option<super::plugins::PluginScan>,
    /// Editors of the plugin instances and their windows.
    pub editors: super::editors::PluginEditors,
    /// Plugin parameter last moved in a plugin window, and its value.
    pub last_plugin_param: Option<(ponzik_engine::AutoTarget, f32)>,
    /// Entrées du navigateur, reconstruites seulement quand leurs sources
    /// (échantillons, plugins, langue) changent, pas à chaque image.
    pub(super) browser_cache: (Option<BrowserKey>, Vec<BrowserEntry>),

    pub playing: bool,
    pub metronome: bool,
    /// Position de lecture en ticks.
    pub position: f64,
    pub channel_meters: Vec<Meter>,
    pub track_meters: Vec<Meter>,
    pub scope: Vec<f32>,
    pub scope_at: usize,
    /// Échantillons du master arrivés pendant cette image.
    scope_new: Vec<f32>,
    /// Sonie (BS.1770) et spectre du master, calculés sur le flux de l'oscilloscope.
    pub loudness: LoudnessMeter,
    pub spectrum: SpectrumAnalyzer,
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
    /// Gesture on the arrangement or tempo lane.
    pub arrange_drag: Option<super::arrange::ArrangeDrag>,
    /// Length of the count-in in progress, in ticks (one bar of the signature).
    pub count_bar: f64,
    /// Note de pré-écoute à relâcher : (canal, note).
    pub preview: Option<(usize, u8)>,
    /// Notes tenues au clavier d'ordinateur ou au contrôleur.
    pub held: Vec<super::keys::HeldNote>,
    pub keyboard_octave: i32,
    /// Enregistrement armé : les notes jouées pendant la lecture sont écrites.
    pub record: bool,
    /// Pré-compte d'une mesure au métronome quand la lecture part enregistrement armé.
    pub count_in: bool,
    /// Pré-compte restant, en ticks, lu dans le moteur (0 hors pré-compte).
    pub counting: f64,
    /// Instants (s) des derniers appuis de tap tempo.
    taps: Vec<f32>,
    /// Enregistrement audio : entrée, pistes armées, prises, panneau.
    pub rec: super::record::RecordState,
    /// Piste en solo (non enregistrée) : les autres sont coupées dans le moteur.
    pub solo: Option<usize>,
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
    pub midi_out: super::midiout::MidiOutState,
    pub tips: TipState,
    /// Glisser-déposer depuis le navigateur, et les zones de dépôt de l'image.
    pub dnd: Option<super::dnd::Dnd>,
    pub drops: Vec<(Rect, super::dnd::DropTarget)>,
    /// Défilement au bouton du milieu : zone, point de départ, défilement d'origine (x, y).
    pub pan: Option<(u64, f32, f32, f32, f32)>,
    /// Catégorie affichée par le navigateur.
    pub sidebar_tab: usize,
    /// Sauvegarde automatique (absente hors de l'interface, en capture par exemple).
    autosaver: Option<Autosaver>,
    /// Sauvegarde d'une session interrompue, proposée au démarrage : fichier, ligne de détail.
    pub recovery: Option<(PathBuf, String)>,
    pub tour: super::tour::Tour,
    /// Place de la pastille de gamme, où s'ouvre son menu.
    scale_anchor: Rect,
    /// Tailles des zones et fenêtres d'appareil flottantes (non enregistrées).
    pub layout: super::float::Layout,
    /// Forme du curseur voulue par l'image en cours (flèche par défaut) ;
    /// la boucle principale l'applique à la fenêtre.
    pub cursor: ponzik_window::Cursor,
    /// Espace de travail et raccourcis choisis.
    pub prefs: super::settings::Prefs,
    /// Atelier : échantillon ouvert et son édition.
    pub lab: super::samplelab::Lab,
    /// Device open in each rack: (track, slot) → unit.
    pub rack_sel: HashMap<(usize, usize), usize>,
    /// Device chip dragged inside a rack.
    pub rack_chip: Option<super::rackfx::ChipDrag>,
    /// Modulator cards: handle drag, depth mode, live traces.
    pub mod_ui: super::modulators::ModUi,
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
        let mut app = Self {
            engine,
            synced: project.clone(),
            project,
            past: Vec::new(),
            future: Vec::new(),
            last_edit: None,
            modified: false,
            project_path: None,
            import_report: Vec::new(),
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
                scale_snap: false,
                chord_stamp: false,
                chord: None,
                chord_inv: 0,
                voicing: ponzik_project::Voicing::Close,
                erased: Vec::new(),
            },
            playlist: PlaylistState { scroll_x: 0.0, zoom: 44.0, brush: 0, selected: None },
            sidebar_scroll: 0.0,
            sidebar_query: String::new(),
            sidebar_focus: false,
            chain_scroll: 0.0,
            mixer_scroll: 0.0,
            mixer_sel: Vec::new(),
            samples: HashMap::new(),
            waveforms: HashMap::new(),
            responses: HashMap::new(),
            sample_files: compat::scan_samples(),
            library_build: None,
            plugins: Vec::new(),
            vst3_plugins: Vec::new(),
            scan_errors: Vec::new(),
            plugin_scan: None,
            editors: Default::default(),
            last_plugin_param: None,
            xrun_watch: Default::default(),
            browser_cache: (None, Vec::new()),
            playing: false,
            metronome: false,
            position: 0.0,
            channel_meters: vec![Meter::default(); channels],
            track_meters: vec![Meter::default(); TRACKS],
            scope: vec![0.0; SCOPE_LEN],
            scope_at: 0,
            scope_new: Vec::with_capacity(SCOPE_LEN),
            loudness: master_loudness(sample_rate),
            spectrum: master_spectrum(sample_rate),
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
            arrange_drag: None,
            count_bar: TICKS_PER_BAR as f64,
            preview: None,
            held: Vec::new(),
            keyboard_octave: 4,
            record: false,
            count_in: true,
            counting: 0.0,
            taps: Vec::new(),
            typing_keyboard: false,
            session_cols: Default::default(),
            solo: None,
            rec: Default::default(),
            start_marker: 0.0,
            clipboard_notes: Vec::new(),
            clipboard_clip: None,
            time_selection: None,
            midi_inputs: ponzik_midi::open_all_inputs(),
            midi_events: Vec::new(),
            midi_out: Default::default(),
            tips: TipState::default(),
            dnd: None,
            drops: Vec::new(),
            pan: None,
            sidebar_tab: 0,
            autosaver: None,
            recovery: None,
            tour: Default::default(),
            scale_anchor: Rect::new(0.0, 0.0, 0.0, 0.0),
            layout: Default::default(),
            cursor: Default::default(),
            prefs: Default::default(),
            lab: Default::default(),
            rack_sel: HashMap::new(),
            rack_chip: None,
            mod_ui: Default::default(),
        };
        // What the cache knows; the background scan brings the rest.
        app.set_plugins(super::plugins::cached());
        app
    }

    /// The next edit starts its own undo step (a new gesture in a plugin window).
    pub fn break_undo_merge(&mut self) {
        self.last_edit = None;
    }

    /// Ouvre un menu par son nom (captures d'écran).
    pub fn screenshot_popup(&mut self, name: &str) {
        let kind = match name {
            "templates" => Popup::Templates,
            "scale" => Popup::Scale,
            "stamp" => {
                self.piano.chord_stamp = true;
                self.piano.scale_snap = true;
                return;
            }
            "humanize" => Popup::Humanize,
            "sidechain" => {
                // Premier insert à clé de la piste en focus.
                let track = self.focused_track();
                let Some(slot) = self.project.tracks[track]
                    .inserts
                    .iter()
                    .position(|fx| fx.as_ref().is_some_and(|d| registry::takes_sidechain(&d.kind)))
                else {
                    return;
                };
                Popup::Sidechain { track, slot }
            }
            "notefx" => Popup::AddNoteFx { channel: self.selected_channel().unwrap_or(0), slot: 0 },
            _ if name.starts_with("palette:") => {
                Popup::Palette { query: name["palette:".len()..].to_string(), selected: 0 }
            }
            "clip" => {
                self.project.patterns[2].color = 0xf0b44a;
                self.playlist.selected = Some(1);
                self.open_popup(Popup::ClipMenu(1), Rect::new(700.0, 168.0, 0.0, 19.0));
                return;
            }
            "colors" => {
                self.open_popup(Popup::PatternColor(0), Rect::new(330.0, 150.0, 0.0, 19.0));
                return;
            }
            "duptime" => {
                self.time_selection = Some((View::Playlist, 4 * TICKS_PER_BAR, 8 * TICKS_PER_BAR));
                self.duplicate_time();
                return;
            }
            "countin" => {
                self.stop();
                self.record = true;
                self.tap_tempo();
                self.tap_tempo();
                self.start_transport();
                return;
            }
            "history" => {
                for label in [tr("Place a note"), tr("Move notes"), tr("Tempo"), tr("Scale"), tr("Add an effect")] {
                    self.project.bpm += 1.0;
                    self.commit(label);
                    self.last_edit = None;
                }
                self.undo();
                self.undo();
                Popup::History
            }
            "chord" => {
                self.piano.chord_stamp = true;
                Popup::Chord
            }
            "rack-param" => {
                // Right-click on the saturator's drive in the first rack of the focused track.
                let track = self.focused_track();
                let Some(slot) = self.project.tracks[track].inserts.iter().position(|f| f.as_ref().is_some_and(|f| f.kind == "rack")) else { return };
                let id = ponzik_engine::effects::fx_rack::unit_param(1, 2);
                let target = ponzik_engine::AutoTarget::EffectParam { track, slot, id };
                self.open_popup(Popup::Param { target, value: 0.5, default: 0.5 }, Rect::new(980.0, 520.0, 0.0, 0.0));
                return;
            }
            "newbus" => {
                self.open_popup(Popup::NewBus, Rect::new(1600.0 - 128.0, 68.0, 76.0, 36.0));
                return;
            }
            "track" => {
                let t = self.focused_track();
                self.open_popup(Popup::TrackMenu(t), Rect::new(560.0, 146.0, 0.0, 0.0));
                return;
            }
            "settings" | "keys" => Popup::Settings { tab: usize::from(name == "keys") },
            "conflict" => {
                self.screenshot_conflict();
                return;
            }
            "menu" | "themes" => {
                let kind = if name == "menu" { Popup::MainMenu } else { Popup::Themes };
                self.open_popup(kind, Rect::new(1600.0 - 44.0, 8.0, 32.0, 32.0));
                return;
            }
            _ => return,
        };
        self.open_popup(kind, Rect::new(60.0, 8.0, 0.0, TOP_BAR - 12.0));
    }

    // --- Sauvegarde automatique ---

    /// Cherche une session interrompue, puis ouvre la sauvegarde automatique
    /// (toutes les deux minutes, dans le dossier des projets).
    pub fn start_autosave(&mut self) {
        let dir = compat::projects_dir();
        self.recovery = autosave::recoverable(&dir).map(|path| {
            let detail = recovery_detail(&path);
            (path, detail)
        });
        match Autosaver::start(&dir, &self.project, 0.0) {
            Ok(a) => self.autosaver = Some(a),
            Err(e) => eprintln!("autosave: {e}"),
        }
    }

    /// `now` : secondes depuis le démarrage.
    pub fn autosave_tick(&mut self, now: f64) {
        // Plugins are asked for their state only when an autosave is due.
        if self.autosaver.as_ref().is_some_and(|a| a.due(now)) {
            self.capture_plugin_states();
        }
        if let Some(a) = &mut self.autosaver
            && let Err(e) = a.tick(&self.project, now)
        {
            eprintln!("autosave: {e}");
        }
    }

    /// Fin normale : retire le verrou de session.
    pub fn close_autosave(&mut self) {
        if let Some(a) = self.autosaver.take()
            && let Err(e) = a.close()
        {
            eprintln!("autosave: {e}");
        }
    }

    fn mark_saved(&mut self) {
        if let Some(a) = &mut self.autosaver {
            a.mark_saved(&self.project);
        }
    }

    /// Carte de reprise, en haut au centre, si une session a été interrompue.
    fn recovery_rect(&self, w: f32) -> Option<Rect> {
        self.recovery.as_ref().map(|_| Rect::new(((w - 300.0) * 0.5).round(), TOP_BAR + 12.0, 300.0, 56.0))
    }

    fn recovery_prompt(&mut self, c: &mut Canvas, input: &Input, w: f32) {
        let (Some(r), Some((path, detail))) = (self.recovery_rect(w), self.recovery.clone()) else { return };
        let t = self.theme;
        let a = self.anim.to(wid("recover", 0, 0), 1.0, T_LAYOUT);
        let r = r.translate(0.0, ((1.0 - a) * -8.0).round());
        c.fill_rrect(r.translate(0.0, 6.0).inset(-2.0), 16.0, Color::rgba(0, 0, 0, (80.0 * a) as u8));
        c.fill_rrect(r, 14.0, t.raised_hi.mix(t.panel, 0.4).alpha(a));
        c.stroke_rrect(r, 14.0, 1.0, t.hairline.alpha(1.6 * a));
        let cy = r.y + r.h * 0.5;
        c.fill_circle(r.x + 28.0, cy, 16.0, t.accent.alpha(0.14 * a));
        draw_icon(c, Icon::Reset, r.x + 28.0, cy, 16.0, t.accent.alpha(a));
        self.text(c, self.f.sans_med, 14.0, r.x + 54.0, cy - 8.0, tr("Recover?"), t.text.alpha(a));
        self.text_fit(c, self.f.sans, 11.0, r.x + 54.0, cy + 10.0, &detail, r.w - 54.0 - 88.0, t.muted.alpha(a));
        let yes = Rect::new(r.right() - 82.0, cy - 17.0, 34.0, 34.0);
        let no = Rect::new(r.right() - 44.0, cy - 17.0, 34.0, 34.0);
        if self.icon_button(c, input, wid("recover-yes", 0, 0), yes, Icon::Check, true, (tr("Reopen the autosave"), "")) {
            self.recovery = None;
            match compat::load_project(&path) {
                Ok(p) => {
                    self.replace_project(p, None);
                    self.modified = true;
                    self.toast(tr("Project recovered · Ctrl+S to save it"));
                }
                Err(e) => self.toast(&e),
            }
        }
        if self.icon_button(c, input, wid("recover-no", 0, 0), no, Icon::Close, false, (tr("Dismiss"), "")) {
            self.recovery = None;
        }
    }

    // --- Projet, moteur, historique ---

    pub fn send(&mut self, cmd: Command) {
        if let Err(cmd) = self.engine.send(cmd) {
            eprintln!("command queue full: {cmd:?}");
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
            super::assistant::capture(&cmd);
            self.editors.capture(&cmd);
            self.send(cmd);
        }
        self.synced = self.project.clone();
        self.channel_meters.resize(self.project.channels.len(), Meter::default());
    }

    /// À appeler après chaque modification du projet. Les gestes continus (même
    /// libellé à moins d'une seconde) forment une seule étape d'annulation.
    pub fn commit(&mut self, label: &str) {
        // Modulations of a removed or replaced device go with it.
        self.project.prune_modulators();
        if self.project == self.synced {
            return;
        }
        let merge = self.last_edit.as_ref().is_some_and(|(l, t)| l == label && self.time - t < 1.0);
        self.future.clear();
        if !merge {
            self.past.push((label.to_string(), self.synced.clone()));
            trim_history(&mut self.past, HISTORY_CAP, HISTORY_BYTES);
        }
        self.last_edit = Some((label.to_string(), self.time));
        self.modified = true;
        self.sync_engine();
    }

    pub fn undo(&mut self) {
        if let Some((label, prev)) = self.past.pop() {
            let current = std::mem::replace(&mut self.project, prev);
            self.future.push((label.clone(), current));
            self.after_history(&tr_fmt!("Undone: {}", label));
        }
    }

    pub fn redo(&mut self) {
        if let Some((label, next)) = self.future.pop() {
            let current = std::mem::replace(&mut self.project, next);
            self.past.push((label.clone(), current));
            self.after_history(&tr_fmt!("Redone: {}", label));
        }
    }

    /// Annule ou rétablit jusqu'à ce que `n` étapes soient faites.
    pub fn jump_history(&mut self, n: usize) {
        let mut moved = false;
        while self.past.len() > n
            && let Some((label, prev)) = self.past.pop()
        {
            let current = std::mem::replace(&mut self.project, prev);
            self.future.push((label, current));
            moved = true;
        }
        while self.past.len() < n
            && let Some((label, next)) = self.future.pop()
        {
            let current = std::mem::replace(&mut self.project, next);
            self.past.push((label, current));
            moved = true;
        }
        if moved {
            let msg = self.past.last().map_or(tr("Back to the start").to_string(), |(l, _)| tr_fmt!("Back to: {}", l));
            self.after_history(&msg);
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
            .flat_map(|c| c.instrument.sample_paths().cloned())
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
        self.mark_saved();
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

    /// Pose un fichier audio dans la playlist : à (ligne, tick) donnés, sinon au
    /// marqueur de départ sur une nouvelle ligne.
    pub fn add_audio_clip(&mut self, path: &str, at: Option<(usize, u32)>) {
        if !self.ensure_sample(path) {
            return;
        }
        let secs = self.samples[path].duration() as f64;
        let ticks = (secs * self.project.bpm as f64 / 60.0 * ponzik_engine::PPQ as f64).round().max(24.0) as u32;
        let name = std::path::Path::new(path).file_stem().map_or(tr("Audio").to_string(), |s| s.to_string_lossy().into_owned());
        let used: Vec<usize> = self.project.channels.iter().map(|c| c.track).collect();
        let track = (1..TRACKS).find(|t| !used.contains(t) && !self.track_in_use(*t)).unwrap_or(1);
        self.project.tracks[track].name = name.clone();
        let lane = self.project.clips.iter().map(|c| c.lane + 1).chain(self.project.audio_clips.iter().map(|c| c.lane + 1)).max().unwrap_or(0);
        let start = (self.start_marker.max(0.0) as u32 / 96) * 96;
        let (lane, start) = at.unwrap_or((lane, start));
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
            take: 0,
            muted: false,
        });
        self.commit(tr("Place an audio clip"));
        self.toast(&tr_fmt!("{} placed in the playlist", name));
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
        let idx = self.project.add_channel(tr(info.name), InstrumentDef::new(kind), 60);
        self.focus = Focus::Channel(idx);
        self.commit(tr("Add a channel"));
        self.toast(&tr_fmt!("{} added to the rack", tr(info.name)));
    }

    pub fn add_sample_channel(&mut self, path: &str) {
        if !self.ensure_sample(path) {
            return;
        }
        let name = std::path::Path::new(path)
            .file_stem()
            .map_or(tr("Sample").to_string(), |s| s.to_string_lossy().into_owned());
        let mut def = InstrumentDef::new("sampler");
        def.sample = Some(path.to_string());
        if registry::instrument_kind("sampler").is_none() {
            self.toast(tr("The sampler is not available yet"));
            return;
        }
        let idx = self.project.add_channel(name.clone(), def, 60);
        self.focus = Focus::Channel(idx);
        self.commit(tr("Add a sample"));
        self.toast(&tr_fmt!("{} added to the rack", name));
    }

    pub fn add_effect(&mut self, track: usize, kind: &str) {
        let Some(info) = registry::effect_kind(kind) else { return };
        if self.project.tracks[track].kind == TrackKind::Vca {
            self.toast(tr("A VCA carries no sound: put the effect on a bus"));
            return;
        }
        let Some(slot) = self.project.tracks[track].inserts.iter().position(Option::is_none) else {
            self.toast(tr("No free slot left on this track"));
            return;
        };
        self.project.tracks[track].inserts[slot] = Some(EffectDef::new(kind));
        self.commit(tr("Add an effect"));
        let name = self.project.tracks[track].name.clone();
        self.toast(&tr_fmt!("{} added on {}", tr(info.name), name));
    }

    /// Solo d'une piste : elle, ses destinations jusqu'au master, et rien d'autre.
    /// Un second appui (ou un autre solo) rétablit l'état muet du projet.
    pub fn toggle_solo(&mut self, track: usize) {
        self.solo = if self.solo == Some(track) { None } else { Some(track) };
        let keep = self.solo.map(|solo| self.solo_keep(solo)).unwrap_or_default();
        for t in 0..TRACKS {
            let muted = if self.solo.is_some() { !keep.contains(&t) } else { self.project.tracks[t].muted };
            self.send(Command::SetTrackMute { track: t, muted: muted || self.project.tracks[t].muted });
        }
    }

    /// Pistes qui restent audibles quand `solo` est en solo : elle et ses
    /// destinations jusqu'au master, tout ce qui y sort (bus imbriqués compris),
    /// les pistes d'un VCA en solo, et les VCA qui pilotent ce qui reste.
    pub(super) fn solo_keep(&self, solo: usize) -> Vec<usize> {
        let p = &self.project;
        let mut keep = vec![MASTER, solo];
        let mut roots = vec![solo];
        if p.tracks[solo].kind == TrackKind::Vca {
            roots.extend(p.vca_members(solo));
        }
        let reaches = |mut t: usize, target: usize| {
            for _ in 0..TRACKS {
                if t == target {
                    return true;
                }
                let out = p.tracks[t].output;
                if out == t || t == MASTER {
                    return false;
                }
                t = out;
            }
            false
        };
        for &r in &roots {
            // Vers le master, puis tout ce qui y sort.
            let mut t = r;
            for _ in 0..TRACKS {
                keep.push(t);
                let out = p.tracks[t].output;
                if out == t || t == MASTER {
                    break;
                }
                t = out;
            }
            keep.extend((1..TRACKS).filter(|&i| i != r && reaches(i, r)));
        }
        let vcas: Vec<usize> = keep.iter().filter_map(|&t| p.tracks[t].vca).collect();
        keep.extend(vcas);
        keep.sort_unstable();
        keep.dedup();
        keep
    }

    /// Renders every section (marker to marker) to its own WAV file.
    pub fn export_sections(&mut self, options: ponzik_project::ExportOptions) {
        self.capture_plugin_states();
        let result = {
            let loader = self.sample_loader();
            compat::export_regions(&self.project, self.sample_rate, &loader, options)
        };
        match result {
            Ok((dir, n)) => self.toast(&ponzik_i18n::tr_fmt_n!("{} section exported to {}", "{} sections exported to {}", n, n, dir.display().to_string())),
            Err(e) => self.toast(&e),
        }
    }

    pub fn export_stems(&mut self) {
        self.capture_plugin_states();
        let result = {
            let loader = self.sample_loader();
            compat::export_stems(&self.project, self.sample_rate, &loader)
        };
        match result {
            Ok((dir, n)) => self.toast(&tr_fmt!("{} stems exported to {}", n, dir.display().to_string())),
            Err(e) => self.toast(&e),
        }
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
            self.commit(tr("Playback mode"));
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
        self.commit(tr("Change pattern"));
    }

    /// Lecture / pause : la pause garde la position.
    pub fn toggle_play(&mut self) {
        if self.playing {
            self.playing = false;
            self.send(Command::Stop);
        } else {
            self.start_transport();
        }
    }

    /// Lance le transport : enregistrement armé, une mesure de pré-compte d'abord.
    fn start_transport(&mut self) {
        self.playing = true;
        if self.record && self.count_in {
            let bar = self.sig_here().bar_len();
            self.count_bar = bar as f64;
            self.counting = bar as f64;
            self.send(Command::CountIn(bar));
        } else {
            self.send(Command::Play);
        }
    }

    /// Tap tempo : le tempo suit la moyenne des intervalles entre les 4 derniers
    /// appuis ; une pause de plus de 2 s repart de zéro.
    pub fn tap_tempo(&mut self) {
        let now = self.time;
        if self.taps.last().is_some_and(|&last| now - last > 2.0) {
            self.taps.clear();
        }
        self.taps.push(now);
        if self.taps.len() > 4 {
            self.taps.remove(0);
        }
        self.anim.set(wid("tb-tap-flash", 0, 0), 1.0);
        if let Some(bpm) = tap_bpm(&self.taps) {
            let bpm = ((bpm * 10.0).round() / 10.0).clamp(40.0, 300.0);
            if self.set_tempo_here(bpm) {
                self.commit(tr("Tempo"));
            }
        }
    }

    pub fn toggle_count_in(&mut self) {
        self.count_in = !self.count_in;
        self.toast(if self.count_in { tr("One-bar count-in on") } else { tr("Count-in off") });
    }

    /// Lecture / arrêt façon FL Studio (Espace) : l'arrêt revient au marqueur de départ.
    pub fn play_stop(&mut self) {
        if self.playing {
            self.stop();
        } else {
            self.send(Command::Locate(self.start_marker));
            self.start_transport();
            // Nouvelle écoute : la sonie intégrée repart de zéro.
            self.loudness.reset();
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
        snap: &dyn Fn(f32) -> f32,
        x_of: &dyn Fn(f32) -> f32,
        tick_at: &dyn Fn(f32) -> f32,
    ) -> Option<(u32, u32)> {
        let t = self.theme;
        let id = wid("ruler", view as usize, 0);
        let hov = self.hovered(input, id, ruler);
        let snap_t = |x: f32| snap(tick_at(x).max(0.0)) as u32;
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
    pub(super) fn set_loop(&mut self, view: View, range: Option<(u32, u32)>) {
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
        self.capture_plugin_states();
        let path = self.project_path.clone().unwrap_or_else(|| compat::default_project_path(&self.project.name));
        match compat::save_project(&self.project, &path) {
            Ok(()) => {
                self.project_path = Some(path.clone());
                self.modified = false;
                self.mark_saved();
                self.toast(&tr_fmt!("Saved to {}", path.display().to_string()));
            }
            Err(e) => self.toast(&e),
        }
    }

    pub fn open_file(&mut self, path: PathBuf) {
        if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("flp")) {
            return self.import_flp(&path);
        }
        match compat::load_project(&path) {
            Ok(p) => {
                let name = p.name.clone();
                self.replace_project(p, Some(path));
                self.toast(&tr_fmt!("{} opened", name));
            }
            Err(e) => self.toast(&e),
        }
    }

    /// Opens an FL Studio project as a new, unsaved Ponzik project.
    pub fn import_flp(&mut self, path: &std::path::Path) {
        match compat::import_flp(path) {
            Ok((p, report)) => {
                self.replace_project(p, None);
                self.import_report = report.rows(46);
                self.toast(&report.summary());
                let anchor = Rect::new(12.0, TOP_BAR, 0.0, 0.0);
                self.open_popup(Popup::ImportReport, anchor);
            }
            Err(e) => self.toast(&e),
        }
    }

    pub fn export(&mut self) {
        self.export_with(ponzik_project::ExportOptions::default());
    }

    pub fn export_with(&mut self, options: ponzik_project::ExportOptions) {
        // The render builds its own instances: they start from the states of now.
        self.capture_plugin_states();
        let path = compat::default_export_path(&self.project.name);
        let result = {
            let loader = self.sample_loader();
            compat::export_song(&self.project, self.sample_rate, &loader, &path, options)
        };
        match result {
            Ok((secs, gain)) if gain.abs() >= 0.05 => {
                self.toast(&tr_fmt!("Exported: {} ({:.1} s, {:+.1} dB)", path.display().to_string(), secs, gain));
            }
            Ok((secs, _)) => self.toast(&tr_fmt!("Exported: {} ({:.1} s)", path.display().to_string(), secs)),
            Err(e) => self.toast(&e),
        }
    }

    // --- Image ---

    fn poll_engine(&mut self, dt: f32) {
        self.engine.collect_garbage();
        self.record_tick(dt);
        self.position = self.engine.position();
        // Tant que le moteur n'a pas reçu l'ordre de lecture, le pré-compte demandé reste affiché.
        let counting = self.engine.count_in();
        if !self.playing {
            self.counting = 0.0;
        } else if counting > 0.0 || self.engine.is_playing() {
            self.counting = counting;
        }
        for (i, m) in self.channel_meters.iter_mut().enumerate() {
            m.feed(self.engine.take_channel_peak(i), dt);
        }
        for (i, m) in self.track_meters.iter_mut().enumerate() {
            m.feed(self.engine.take_track_peak(i), dt);
        }
        let load = self.engine.take_load();
        self.load += (load - self.load) * (1.0 - (-dt / 0.4).exp());
        let (scope, at, new) = (&mut self.scope, &mut self.scope_at, &mut self.scope_new);
        new.clear();
        self.engine.drain_scope(|s| {
            scope[*at] = s;
            *at = (*at + 1) % SCOPE_LEN;
            new.push(s);
        });
        if !self.scope_new.is_empty() {
            self.loudness.process(&[&self.scope_new]);
            self.spectrum.push(&self.scope_new);
        }
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
            || self.anim.is_ambient()
            || self.drag.is_some()
            || !self.toasts.is_empty()
            || self.tips.waiting
            || self.dnd.is_some()
            || self.engine.audition_position().is_some()
            || self.track_meters.iter().any(Meter::active)
            || self.channel_meters.iter().any(Meter::active)
            || self.rec.needs_redraw()
    }

    /// Vrai si l'image doit suivre à 60 images/s : animation, glisser, défilement.
    /// La lecture seule (tête, VU-mètres) se contente de 30.
    pub fn wants_smooth_frames(&self) -> bool {
        self.anim.is_moving() || self.drag.is_some() || self.dnd.is_some() || self.pan.is_some()
    }

    /// Raccourcis globaux, ceux de la table des touches (FL Studio par défaut).
    fn shortcuts(&mut self, input: &Input) {
        for &(key, mods) in &input.keys {
            if self.view == View::Lab && self.lab_key(key, mods) {
                continue;
            }
            if key == Key::Escape {
                self.popup = None;
                self.mixer_sel.clear();
                self.piano.selected.clear();
                self.playlist.selected = None;
                if let Some((v, _, _)) = self.time_selection.take() {
                    self.set_loop(v, None);
                }
                continue;
            }
            // Ctrl+G dans le mixer : grouper dans un bus.
            if key == Key::Char('g') && mods.ctrl && self.view == View::Mixer && self.prefs.keymap.lookup(key, mods).is_none() {
                let group = self.group_for(self.focused_track());
                self.new_bus(&group);
                continue;
            }
            let Some(s) = self.prefs.keymap.lookup(key, mods) else {
                // Ctrl+Maj+Z rétablit aussi, comme ailleurs.
                if key == Key::Char('z') && mods.ctrl && mods.shift {
                    self.redo();
                }
                continue;
            };
            // Clavier musical actif : les lettres seules jouent des notes.
            if self.typing_keyboard && self.prefs.keymap.get(s).is_some_and(super::keymap::Combo::is_plain_char) {
                continue;
            }
            self.run_shortcut(s);
        }
    }

    pub fn open_palette(&mut self) {
        self.open_popup(Popup::Palette { query: String::new(), selected: 0 }, Rect::new(0.0, 0.0, 0.0, 0.0));
    }

    pub fn toggle_typing_keyboard(&mut self) {
        self.typing_keyboard = !self.typing_keyboard;
        self.release_all_held();
        let key = self.key_label(super::keymap::Shortcut::TypingKeyboard);
        self.toast(&if self.typing_keyboard {
            tr_fmt!("Musical typing on: letters play notes ({} to go back)", key)
        } else {
            tr("Musical typing off").into()
        });
    }

    pub fn set_view(&mut self, v: View) {
        if !self.view_shown(v) {
            self.toast(&tr_fmt!("{} is hidden · ☰ Settings to show it", v.label()));
            return;
        }
        if v != self.view {
            if self.view == View::Lab {
                self.lab_stop();
            }
            self.view = v;
            self.anim.set(wid("view-enter", 0, 0), 0.0);
        }
    }

    pub fn frame(&mut self, c: &mut Canvas, input: &Input, dt: f32) {
        self.anim.begin_frame(dt);
        self.time += dt;
        self.cursor = ponzik_window::Cursor::Arrow;
        self.poll_engine(dt);
        if self.popup.is_none() && !self.sidebar_focus {
            self.shortcuts(input);
            self.musical_keyboard(input);
        } else if self.popup.is_some() && !self.settings_open() && input.keys.iter().any(|(k, _)| *k == Key::Escape) {
            self.popup = None;
        }
        if input.focus_lost {
            self.release_all_held();
        }

        let t = self.theme;
        let (w, h) = c.logical_size();
        c.set_clip(None);
        c.clear(t.bg);

        // Un menu ouvert capte la souris : le reste de l'écran ne réagit plus.
        let masked;
        let over_card = self.recovery_rect(w).is_some_and(|r| r.contains(input.x, input.y)) || (self.popup.is_none() && self.over_tour(input));
        let ui = if self.popup.is_some() || over_card {
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
        // Séparateurs et fenêtres flottantes : tailles des zones, et souris captée au-dessus.
        let (sb, pn, covered) = self.layout_begin(ui, w, h);
        let covered = covered || self.record_covers(ui);
        let below;
        let under = if covered {
            below = super::float::masked(ui);
            &below
        } else {
            ui
        };

        let main = Rect::new(sb, TOP_BAR, w - sb, h - TOP_BAR - pn);
        self.main_area(c, under, main);
        if pn > 1.0 {
            self.panel_area(c, under, Rect::new(sb, h - pn, w - sb, pn));
        }
        if sb > 1.0 {
            self.sidebar_area(c, under, Rect::new(0.0, TOP_BAR, sb, h - TOP_BAR));
        }
        self.top_bar(c, under, Rect::new(0.0, 0.0, w, TOP_BAR));
        self.float_layer(c, ui);
        self.record_panel(c, ui, Rect::new(0.0, 0.0, w, h));
        if self.popup.is_none() {
            self.recovery_prompt(c, input, w);
        }
        self.popup_layer(c, input, Rect::new(0.0, 0.0, w, h));
        if self.popup.is_none() {
            self.tour_layer(c, input, Rect::new(0.0, 0.0, w, h));
        } else {
            self.tour.anchors = Default::default();
        }
        self.toast_layer(c, dt, Rect::new(sb, TOP_BAR, w - sb, h - TOP_BAR - pn));
        self.dnd_layer(c, input);
        if self.popup.is_none() {
            self.tooltip_layer(c, input, Rect::new(0.0, 0.0, w, h));
        } else {
            self.tips = TipState::default();
        }

        if !input.down {
            self.drag = None;
            self.release_preview();
        }
        if !input.middle_down {
            self.pan = None;
        }
    }

    // --- Barre du haut ---

    fn top_bar(&mut self, c: &mut Canvas, input: &Input, r: Rect) {
        let t = self.theme;
        c.fill_rect(r, t.bg_deep);
        c.fill_rect(Rect::new(r.x, r.bottom() - 1.0, r.w, 1.0), t.hairline);
        let cy = r.y + r.h * 0.5;

        use super::keymap::Shortcut as S;
        use super::settings::Bar;
        let key = |s: S| self.key_label(s);
        let (k_play, k_mode, k_tap, k_metro, k_rec, k_keys) =
            (key(S::PlayStop), key(S::ToggleMode), key(S::TapTempo), key(S::Metronome), key(S::Record), key(S::TypingKeyboard));
        let k_browser = key(S::ToggleBrowser);
        let (k_pal, k_save, k_redo, k_undo, k_panel, k_hist) =
            (key(S::Palette), key(S::Save), key(S::Redo), key(S::Undo), key(S::TogglePanel), key(S::History));
        if self.icon_button(c, input, wid("tb-sidebar", 0, 0), Rect::new(12.0, cy - 16.0, 32.0, 32.0), Icon::Sidebar, self.sidebar, (tr("Browser"), &k_browser)) {
            self.sidebar = !self.sidebar;
        }
        // Marque : le nom et sa LED, qui s'allume quand ça joue.
        let led = self.anim.to(wid("brand-led", 0, 0), self.playing as u8 as f32, T_HEAD);
        c.fill_circle(58.0, cy, 4.0, t.raised_hi.mix(t.accent, led.max(0.35)));
        let wordmark = self.text(c, self.f.sans_med, 18.0, 70.0, cy, "Ponzik", t.text);

        // Transport, centré ; l'espace de travail choisit ses éléments.
        let on = |b: Bar| self.bar_shown(b);
        let widths = [(Bar::Mode, 142.0), (Bar::Pattern, 198.0), (Bar::Tap, 50.0), (Bar::Position, 138.0), (Bar::Scale, 134.0), (Bar::Metronome, 40.0), (Bar::Record, 80.0), (Bar::Keyboard, 40.0)];
        let cluster = 46.0 + 54.0 + 120.0 - 4.0 + widths.iter().filter(|(b, _)| on(*b)).map(|(_, w)| w).sum::<f32>();
        let mut x = ((r.w - cluster) * 0.5).round().max(70.0 + wordmark + 24.0);
        let start = x;

        let play = Rect::new(x, cy - 18.0, 40.0, 36.0);
        let hov = self.hovered(input, wid("tb-play", 0, 0), play);
        if hov && input.pressed {
            self.toggle_play();
        }
        if hov {
            self.set_tip(if self.playing { tr("Pause") } else { tr("Play") }, &k_play);
        }
        let hv = self.hover_anim(wid("tb-play", 0, 0), hov);
        let on = self.anim.to(wid("tb-play-on", 0, 0), self.playing as u8 as f32, T_HOVER);
        c.fill_rrect(play, 10.0, t.text.mix(t.accent, on).mix(WHITE, 0.12 * hv));
        c.fill_rect(Rect::new(play.x + 8.0, play.y + 1.0, play.w - 16.0, 1.0), WHITE.alpha(0.3));
        let (px, py) = play.center();
        draw_icon(c, if self.playing { Icon::Pause } else { Icon::Play }, px, py, 16.0, t.bg_deep.mix(t.on_accent, on));
        x += 46.0;
        if self.icon_button(c, input, wid("tb-stop", 0, 0), Rect::new(x, cy - 18.0, 40.0, 36.0), Icon::Stop, false, (tr("Stop"), "")) {
            self.stop();
        }
        x += 54.0;

        // Mode de lecture : l'icône de la vue où l'on construit ce qui joue.
        let mode = match self.project.mode {
            PlayMode::Pattern(_) => 0,
            PlayMode::Song => 1,
            PlayMode::Session => 2,
        };
        let icons = [Icon::Rack, Icon::Playlist, Icon::Session];
        let tips = [(tr("Play the pattern"), k_mode.as_str()), (tr("Play the song"), k_mode.as_str()), (tr("Play the session"), "")];
        if self.bar_shown(Bar::Mode) {
            if let Some(m) = self.icon_segmented(c, input, wid("tb-mode", 0, 0), Rect::new(x, cy - 18.0, 132.0, 36.0), &icons, &tips, mode) {
                let cur = self.project.current_pattern;
                self.set_mode(match m {
                    0 => PlayMode::Pattern(cur),
                    1 => PlayMode::Song,
                    _ => PlayMode::Session,
                });
                if m == 2 && self.view_shown(View::Session) {
                    self.set_view(View::Session);
                }
            }
            x += 142.0;
        }
        if self.bar_shown(Bar::Pattern) {
            self.pattern_selector(c, input, Rect::new(x, cy - 18.0, 188.0, 36.0));
            x += 198.0;
        }

        // While the song plays, the tempo heard (ramps included); otherwise the one edited.
        let heard = if self.playing && self.project.mode == PlayMode::Song {
            self.project.tempo_map().bpm_at(self.position)
        } else {
            self.tempo_here() as f64
        };
        let bpm_text = ponzik_i18n::num(heard, 1);
        let bpm_r = Rect::new(x, cy - 18.0, 116.0, 36.0);
        if let Some(bpm) = self.drag_field(c, input, wid("tb-bpm", 0, 0), bpm_r, self.tempo_here(), 0.25, 1.0, &bpm_text, "BPM") {
            let bpm = ((bpm * 10.0).round() / 10.0).clamp(40.0, 300.0);
            if self.set_tempo_here(bpm) {
                self.commit(tr("Tempo"));
            }
        }
        self.tip_zone(input, bpm_r, tr("Tempo at the playhead\nDrag or scroll · Shift: fine"), "");
        x += 120.0;
        if self.bar_shown(Bar::Tap) {
            self.tap_button(c, input, Rect::new(x, cy - 18.0, 40.0, 36.0), &k_tap);
            x += 50.0;
        }
        if self.bar_shown(Bar::Position) {
            let pos_r = Rect::new(x, cy - 18.0, 132.0, 36.0);
            self.position_field(c, pos_r);
            self.tip_zone(input, pos_r, tr("Position\nbar . beat . step"), "");
            x += 138.0;
        }
        if self.bar_shown(Bar::Scale) {
            self.scale_chip(c, input, Rect::new(x, cy - 18.0, 128.0, 36.0));
            x += 134.0;
        }
        if self.bar_shown(Bar::Metronome) {
            if self.icon_button(c, input, wid("tb-metro", 0, 0), Rect::new(x, cy - 18.0, 36.0, 36.0), Icon::Metronome, self.metronome, (tr("Metronome"), &k_metro)) {
                self.metronome = !self.metronome;
                let m = self.metronome;
                self.send(Command::SetMetronome(m));
            }
            x += 40.0;
        }
        if self.bar_shown(Bar::Record) {
            self.record_button(c, input, Rect::new(x, cy - 18.0, 36.0, 36.0), &k_rec);
            x += 40.0;
            let mic = Rect::new(x, cy - 18.0, 36.0, 36.0);
            if self.icon_button(c, input, wid("tb-input", 0, 0), mic, Icon::Mic, self.rec.panel, (tr("Recording panel: input, level, monitoring, latency"), "")) {
                self.toggle_record_panel((mic.x + mic.w * 0.5, mic.bottom() + 6.0));
            }
            // Témoin de niveau d'entrée sous l'icône, quand une piste est armée.
            if !self.rec.armed.is_empty() {
                let lv = self.rec.input_level();
                let col = if self.rec.clipped { self.theme.negative } else { self.theme.accent };
                c.fill_rrect(Rect::new(mic.x + 8.0, mic.bottom() - 5.0, (mic.w - 16.0) * lv, 2.0), 1.0, col);
            }
            x += 40.0;
        }
        if self.bar_shown(Bar::Keyboard) {
            if self.icon_button(c, input, wid("tb-keys", 0, 0), Rect::new(x, cy - 18.0, 36.0, 36.0), Icon::Keyboard, self.typing_keyboard, (tr("Musical typing"), &k_keys)) {
                self.toggle_typing_keyboard();
            }
            x += 40.0;
        }
        let cluster_end = x - 4.0;

        // Droite : fichier et historique, puis la charge DSP si la place le permet.
        let mut rx = r.w - 12.0 - 32.0;
        let buttons = [
            (Icon::Menu, "tb-menu", (tr("Menu"), "")),
            (Icon::Search, "tb-search", (tr("Find anything"), k_pal.as_str())),
            (Icon::Panel, "tb-panel", (tr("Devices"), k_panel.as_str())),
            (Icon::Save, "tb-save", (tr("Save"), k_save.as_str())),
            (Icon::Redo, "tb-redo", (tr("Redo"), k_redo.as_str())),
            (Icon::Undo, "tb-undo", (tr("Undo"), k_undo.as_str())),
            (Icon::History, "tb-history", (tr("History"), k_hist.as_str())),
        ];
        for (icon, name, tip) in buttons {
            if rx < cluster_end + 8.0 {
                break;
            }
            let hidden = match icon {
                Icon::History => !self.bar_shown(Bar::History),
                Icon::Undo | Icon::Redo => !self.bar_shown(Bar::Undo),
                _ => false,
            };
            if hidden {
                continue;
            }
            let on = icon == Icon::Panel && self.panel;
            if icon == Icon::Menu {
                self.tour_anchor(2, Rect::new(rx, cy - 16.0, 32.0, 32.0));
            }
            if self.icon_button(c, input, wid(name, 0, 0), Rect::new(rx, cy - 16.0, 32.0, 32.0), icon, on, tip) {
                let anchor = Rect::new(rx, cy - 16.0, 32.0, 32.0);
                match icon {
                    Icon::Menu => self.open_popup(Popup::MainMenu, anchor),
                    Icon::Search => self.open_palette(),
                    Icon::Panel => self.panel = !self.panel,
                    Icon::Save => self.save(),
                    Icon::Redo => self.redo(),
                    Icon::Undo => self.undo(),
                    Icon::History => self.open_popup(Popup::History, anchor),
                    _ => {}
                }
            }
            rx -= 36.0;
        }
        // Charge DSP : une jauge discrète, la valeur en infobulle.
        if self.bar_shown(Bar::Load) && rx + 32.0 - 40.0 > cluster_end + 16.0 {
            let gauge = Rect::new(rx + 32.0 - 34.0, cy - 3.0, 26.0, 6.0);
            let load = self.load.clamp(0.0, 1.0);
            c.fill_rrect(gauge, 3.0, t.raised);
            if load > 0.005 {
                let col = if load > 0.85 { t.negative } else { t.muted.mix(t.text2, load) };
                c.fill_rrect(Rect::new(gauge.x, gauge.y, (gauge.w * load).max(6.0), gauge.h), 3.0, col);
            }
            let tip = format!("{}\n{}", tr_fmt!("DSP load: {:.0}%", self.load * 100.0), self.audio_status());
            self.tip_zone(input, gauge.inset(-8.0), &tip, "");
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
            if hov {
                self.set_tip(tr("Project\nDouble-click: rename"), "");
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
        use super::keymap::Shortcut as S;
        let (k_prev, k_next, k_new) = (self.key_label(S::PrevPattern), self.key_label(S::NextPattern), self.key_label(S::NewPattern));
        if self.icon_button(c, input, wid("pat-prev", 0, 0), prev, Icon::ChevronLeft, false, (tr("Previous pattern"), &k_prev)) && cur > 0 {
            self.select_pattern(cur - 1);
        }
        let next_tip = if cur + 1 < n { (tr("Next pattern"), k_next.as_str()) } else { (tr("New pattern"), k_new.as_str()) };
        if self.icon_button(c, input, wid("pat-next", 0, 0), next, Icon::ChevronRight, false, next_tip) {
            if cur + 1 < n {
                self.select_pattern(cur + 1);
            } else {
                let p = self.project.add_pattern();
                self.select_pattern(p);
                self.toast(tr("New pattern"));
            }
        }
        let mid = Rect::new(prev.right(), r.y, next.x - prev.right(), r.h);
        let id = wid("pat-name", 0, 0);
        let hov = self.hovered(input, id, mid);
        let hv = self.hover_anim(id, hov);
        if hov && (input.pressed || input.right_pressed) {
            self.open_popup(Popup::Patterns, r);
        }
        if hov {
            self.set_tip(tr("Patterns"), "");
        }
        if hov && input.wheel != 0.0 {
            let p = (cur as i32 - input.wheel.signum() as i32).clamp(0, n as i32 - 1) as usize;
            if p != cur {
                self.select_pattern(p);
            }
        }
        let name = self.project.patterns[cur].name.clone();
        let (cx, cy) = mid.center();
        c.fill_rrect(Rect::new(mid.x + 6.0, cy - 6.0, 3.0, 12.0), 1.5, self.pattern_tint(cur));
        let label = self.fit(self.f.sans_med, 13.0, &name, mid.w - 22.0);
        self.text_center(c, self.f.sans_med, 13.0, cx + 4.0, cy, &label, t.text2.mix(t.text, 0.6 + 0.4 * hv));
    }

    /// Pastille de la gamme du projet : clic pour la choisir, molette pour la tonalité.
    fn scale_chip(&mut self, c: &mut Canvas, input: &Input, r: Rect) {
        let t = self.theme;
        self.scale_anchor = r;
        let id = wid("tb-scale", 0, 0);
        let hov = self.hovered(input, id, r);
        let hv = self.hover_anim(id, hov);
        c.fill_rrect(r, 10.0, t.raised.mix(t.raised_hi, hv));
        c.stroke_rrect(r, 10.0, 1.0, t.hairline);
        let (key, scale) = (self.project.key, self.project.scale);
        let chromatic = scale == ScaleKind::Chromatic;
        let cy = r.y + r.h * 0.5;
        draw_icon(c, Icon::Scale, r.x + 18.0, cy, 15.0, if chromatic { t.muted } else { t.accent });
        let label = scale_label(key, scale);
        self.text_fit(c, self.f.sans_med, 13.0, r.x + 34.0, cy, &label, r.w - 42.0, if chromatic { t.text2 } else { t.text });
        if hov {
            let tip = if chromatic {
                tr("Project scale\nNone: every note").to_string()
            } else {
                tr_fmt!("Project scale\n{} {} · scroll: key", key_name(key), tr(scale.name()).to_lowercase())
            };
            self.set_tip(&tip, "");
        }
        if hov && input.pressed {
            self.open_popup(Popup::Scale, r);
        }
        if hov && input.wheel != 0.0 && !chromatic {
            self.project.key = ((key as i32 + input.wheel.signum() as i32).rem_euclid(12)) as u8;
            self.commit(tr("Key"));
        }
    }

    /// Choix de la gamme : douze toniques (les notes de la gamme pointées), puis
    /// les modes. Renvoie faux quand il se ferme.
    fn scale_popup(&mut self, c: &mut Canvas, input: &Input, anchor: Rect, screen: Rect) -> bool {
        let t = self.theme;
        let open = self.anim.to(wid("popup", 0, 0), 1.0, T_HOVER);
        const GAP: f32 = 4.0;
        let w = 320.0;
        let key_w = (w - 24.0 - 5.0 * GAP) / 6.0;
        let (key_h, row_h) = (36.0, 32.0);
        // Familles de gammes, chacune sous un petit titre, sur deux colonnes.
        let mut groups: Vec<(&str, Vec<ScaleKind>)> = Vec::new();
        for sc in ScaleKind::ALL {
            match groups.last_mut() {
                Some((g, v)) if *g == sc.group() => v.push(sc),
                _ => groups.push((sc.group(), vec![sc])),
            }
        }
        const HEAD: f32 = 22.0;
        let list_h: f32 = groups.iter().map(|(_, v)| HEAD + v.len().div_ceil(2) as f32 * row_h).sum();
        let h = (40.0 + 2.0 * key_h + GAP + 17.0 + list_h + 10.0).min(screen.h - anchor.bottom() - 20.0);
        let x = anchor.x.min(screen.right() - 12.0 - w).max(12.0);
        let r = Rect::new(x, anchor.bottom() + 8.0 + (1.0 - open) * 6.0, w, h);
        c.fill_rrect(r.translate(0.0, 6.0).inset(-2.0), 16.0, Color::rgba(0, 0, 0, (90.0 * open) as u8));
        c.fill_rrect(r, 14.0, t.panel);
        c.stroke_rrect(r, 14.0, 1.0, t.hairline.alpha(1.6));
        self.text(c, self.f.sans_med, 12.0, r.x + 16.0, r.y + 20.0, tr("Scale"), t.muted);

        let (key, scale) = (self.project.key, self.project.scale);
        let chromatic = scale == ScaleKind::Chromatic;
        let over = |b: Rect| input.inside && b.contains(input.x, input.y);
        for k in 0..12u8 {
            let cell = Rect::new(r.x + 12.0 + (k % 6) as f32 * (key_w + GAP), r.y + 36.0 + (k / 6) as f32 * (key_h + GAP), key_w, key_h);
            let hov = over(cell);
            let hv = self.hover_anim(wid("scale-key", k as usize, 0), hov);
            let cur = k == key % 12;
            let fill = if cur && !chromatic { t.accent } else if cur { t.raised_hi } else { t.raised.mix(t.raised_hi, hv).alpha(0.6 + 0.4 * hv) };
            c.fill_rrect(cell, 8.0, fill);
            let ink = if cur && !chromatic { t.on_accent } else { t.text2.mix(t.text, hv.max(cur as u8 as f32)) };
            let (cx, cy) = cell.center();
            self.text_center(c, self.f.sans_med, 13.0, cx, cy - 2.0, key_name(k), ink);
            // Les notes de la gamme : un point sous leur nom.
            if !chromatic && !cur && scale.contains(key, k) {
                c.fill_circle(cx, cell.bottom() - 6.0, 2.0, t.accent.alpha(0.8));
            }
            if hov && input.pressed && !cur {
                self.project.key = k;
                self.commit(tr("Key"));
            }
        }
        let sep_y = r.y + 36.0 + 2.0 * key_h + GAP + 8.0;
        c.fill_rect(Rect::new(r.x + 12.0, sep_y, r.w - 24.0, 1.0), t.hairline);
        let col_w = (r.w - 24.0 - GAP) * 0.5;
        let prev = c.push_clip(Rect::new(r.x, sep_y + 1.0, r.w, r.bottom() - sep_y - 1.0));
        let mut y = sep_y + 4.0;
        let mut cells = Vec::new();
        for (g, list) in &groups {
            self.text(c, self.f.sans_med, 11.0, r.x + 16.0, y + HEAD * 0.5 + 2.0, tr(g), t.muted);
            y += HEAD;
            for (j, &sc) in list.iter().enumerate() {
                cells.push((sc, Rect::new(r.x + 12.0 + (j % 2) as f32 * (col_w + GAP), y + (j / 2) as f32 * row_h, col_w, row_h - 4.0)));
            }
            y += list.len().div_ceil(2) as f32 * row_h;
        }
        for (i, (sc, row)) in cells.into_iter().enumerate() {
            let hov = over(row) && row.bottom() <= r.bottom();
            let hv = self.hover_anim(wid("scale-kind", i, 0), hov);
            let cur = sc == scale;
            if hv > 0.01 || cur {
                c.fill_rrect(row, 8.0, t.raised.mix(t.raised_hi, hv * 0.6).alpha(hv.max(cur as u8 as f32 * 0.8)));
            }
            let cy = row.y + row.h * 0.5;
            if cur {
                c.fill_rrect(Rect::new(row.x + 4.0, cy - 6.0, 3.0, 12.0), 1.5, t.accent);
            }
            let ink = t.text2.mix(t.text, hv.max(cur as u8 as f32));
            self.text_fit(c, self.f.sans_med, 12.0, row.x + 14.0, cy, tr(sc.name()), row.w - 20.0, ink);
            if hov && input.pressed && !cur {
                self.project.scale = sc;
                self.commit(tr("Scale"));
            }
        }
        c.set_clip(Some(prev));
        let clicked = input.pressed || input.right_pressed;
        !clicked || r.contains(input.x, input.y) || anchor.contains(input.x, input.y)
    }

    fn position_field(&mut self, c: &mut Canvas, r: Rect) {
        let t = self.theme;
        c.fill_rrect(r, 10.0, t.raised);
        c.stroke_rrect(r, 10.0, 1.0, t.hairline);
        let (_, cy) = r.center();
        // Pendant le pré-compte : la mesure « -1 », comptée en rouge comme l'enregistrement.
        let counting = self.counting > 0.0;
        let lit_col = if counting { t.negative } else { t.accent };
        // Bars and beats of the signature map (song) or of the first signature.
        let map = self.project.tempo_map();
        let view = if self.project.mode == PlayMode::Song { map.view() } else { map.head() };
        let at = if counting {
            let sig = self.sig_here();
            let pos = (self.count_bar - self.counting).max(0.0);
            let beat = (pos / sig.beat_len() as f64).floor();
            ponzik_engine::BarBeat { bar: 0, beat: beat as u32, tick_in_beat: pos - beat * sig.beat_len() as f64, sig }
        } else {
            view.bar_beat(self.cycle_position())
        };
        let leds = (at.sig.num as usize).min(8);
        let gap = if leds <= 4 { 11.0 } else { 44.0 / leds as f32 * 0.98 + 1.0 };
        for b in 0..leds {
            let lit = self.playing && b == at.beat as usize;
            let v = self.anim.to_ambient(wid("beat-led", b, 0), lit as u8 as f32, T_HEAD);
            let rad = if leds <= 4 { 3.0 } else { 2.3 };
            c.fill_circle(r.x + 14.0 + b as f32 * gap, cy, rad, t.raised_hi.mix(lit_col, v));
        }
        let bar = if counting { "-1".to_string() } else { (at.bar + 1).to_string() };
        let beat = at.beat + 1;
        let step = (at.tick_in_beat / TICKS_PER_STEP as f64).floor() as u32 + 1;
        let text = format!("{bar}.{beat}.{step}");
        let ink = if counting { t.negative } else if self.playing { t.text } else { t.text2 };
        self.text_right(c, self.f.mono_med, 15.0, r.right() - 12.0, cy, &text, ink);
    }

    /// Bouton TAP : chaque appui cale le tempo sur la moyenne des 4 derniers ;
    /// les points dessous comptent les appuis pris en compte.
    fn tap_button(&mut self, c: &mut Canvas, input: &Input, r: Rect, key: &str) {
        let t = self.theme;
        let id = wid("tb-tap", 0, 0);
        let hov = self.hovered(input, id, r);
        let hv = self.hover_anim(id, hov);
        if hov && input.pressed {
            self.tap_tempo();
        }
        let flash = self.anim.to(wid("tb-tap-flash", 0, 0), 0.0, T_LAYOUT);
        let press = self.anim.to(id ^ 0x5052, if hov && input.down { 0.92 } else { 1.0 }, T_PRESS);
        let body = r.scale(press);
        c.fill_rrect(body, 10.0, t.raised.mix(t.raised_hi, hv).mix(t.accent, flash * 0.35));
        c.stroke_rrect(body, 10.0, 1.0, t.hairline.mix(t.accent, flash));
        let (cx, cy) = r.center();
        self.text_center(c, self.f.mono_med, 11.0, cx, cy - 3.0, tr("TAP"), t.text2.mix(t.text, hv).mix(t.accent, flash));
        let fresh = self.taps.last().is_some_and(|&last| self.time - last <= 2.0);
        let n = if fresh { self.taps.len() } else { 0 };
        for k in 0..4 {
            let on = self.anim.to(wid("tb-tap-dot", k, 0), (k < n) as u8 as f32, T_HOVER);
            c.fill_circle(cx - 7.5 + k as f32 * 5.0, cy + 8.0, 1.3, t.raised_hi.mix(t.accent, on));
        }
        if hov {
            self.set_tip(tr("Tap tempo\nTap the beat: average of the last 4 taps"), key);
        }
    }

    /// Bouton d'enregistrement, avec l'anneau du pré-compte.
    fn record_button(&mut self, c: &mut Canvas, input: &Input, rec: Rect, key: &str) {
        let t = self.theme;
        let rid = wid("tb-rec", 0, 0);
        let rhov = self.hovered(input, rid, rec);
        let rhv = self.hover_anim(rid, rhov);
        let on = self.anim.to(wid("tb-rec-on", 0, 0), self.record as u8 as f32, T_HOVER);
        c.fill_rrect(rec, 8.0, t.raised.alpha(rhv.max(on * 0.6)));
        let (rx0, ry0) = rec.center();
        let pulse = if self.record && self.playing { 0.75 + 0.25 * (self.time * 6.0).sin().abs() } else { 1.0 };
        // Lueur : armé, elle respire ; en prise, elle bat.
        if on > 0.01 {
            let glow = on * if self.playing { pulse } else { 0.55 + 0.15 * (self.time * 2.0).sin() };
            for k in 1..=3 {
                c.fill_circle(rx0, ry0, 6.0 + 4.0 * k as f32, t.negative.alpha(0.07 * glow));
            }
        }
        c.fill_circle(rx0, ry0, 6.0, t.muted.mix(t.text2, rhv).mix(t.negative, on).alpha(pulse));
        // Anneau : pré-compte d'une mesure ; il se remplit pendant le décompte.
        let ci = self.anim.to(wid("tb-rec-ci", 0, 0), self.count_in as u8 as f32, T_HOVER);
        if ci > 0.01 {
            let ring = t.muted.mix(t.negative, on).alpha(0.55 * ci);
            c.stroke_circle(rx0, ry0, 10.5, 1.2, ring);
            if self.counting > 0.0 {
                let done = (1.0 - self.counting / self.count_bar.max(1.0)).clamp(0.0, 1.0) as f32;
                let a0 = -std::f32::consts::FRAC_PI_2;
                let pts: Vec<(f32, f32)> = (0..=32)
                    .map(|k| a0 + done * std::f32::consts::TAU * k as f32 / 32.0)
                    .map(|a| (rx0 + 10.5 * a.cos(), ry0 + 10.5 * a.sin()))
                    .collect();
                c.stroke_polyline(&pts, 2.0, t.negative);
            }
        }
        if rhov {
            let tip = if self.count_in {
                tr("Record notes and audio\nOne-bar count-in · right-click: turn off")
            } else {
                tr("Record notes and audio\nNo count-in · right-click: turn on")
            };
            self.set_tip(tip, key);
        }
        if rhov && input.pressed {
            self.record = !self.record;
            self.toast(if self.record { tr("Recording armed") } else { tr("Recording disarmed") });
        }
        if rhov && input.right_pressed {
            self.toggle_count_in();
        }
    }

    // --- Zone principale ---

    fn main_area(&mut self, c: &mut Canvas, input: &Input, r: Rect) {
        let t = self.theme;
        // Onglets des seules vues proposées par l'espace de travail.
        let views = self.shown_views();
        let tabs = Rect::new(r.x + PAD, r.y + 12.0, (54.0 * views.len() as f32).min(r.w - 2.0 * PAD), 36.0);
        let icons: Vec<Icon> = views.iter().map(|v| v.icon()).collect();
        let keys: Vec<String> = views.iter().map(|&v| self.key_label(super::keymap::Shortcut::for_view(v))).collect();
        let tips: Vec<(&str, &str)> = views.iter().zip(&keys).map(|(v, k)| (v.label(), k.as_str())).collect();
        let sel = views.iter().position(|v| *v == self.view).unwrap_or(0);
        if let Some(i) = self.icon_segmented(c, input, wid("tabs", 0, 0), tabs, &icons, &tips, sel) {
            self.set_view(views[i]);
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
            View::Lab => self.lab_view(c, input, header, body_shifted),
        }
        if enter < 0.999 {
            c.fill_rect(Rect::new(r.x, header.y - 2.0, r.w, r.bottom() - header.y + 2.0), t.bg.alpha(1.0 - enter));
        }
        c.set_clip(Some(prev));
    }

    /// État vide : une icône et une ligne courte, centrées.
    pub(super) fn empty_state(&mut self, c: &mut Canvas, r: Rect, icon: Icon, line: &str) {
        let t = self.theme;
        let (cx, cy) = r.center();
        c.fill_circle(cx, cy - 14.0, 22.0, t.raised.alpha(0.7));
        draw_icon(c, icon, cx, cy - 14.0, 20.0, t.muted);
        let line = self.fit(self.f.sans, 13.0, line, r.w - 16.0);
        self.text_center(c, self.f.sans, 13.0, cx, cy + 24.0, &line, t.text2);
    }

    // --- Panneau d'appareils ---

    fn panel_area(&mut self, c: &mut Canvas, input: &Input, r: Rect) {
        let t = self.theme;
        c.fill_rect(r, t.bg_deep);
        c.fill_rect(Rect::new(r.x, r.y, r.w, 1.0), t.hairline);
        let prev = c.push_clip(r);
        let h = self.layout.panel_open.max(r.h);
        self.device_panel(c, input, Rect::new(r.x, r.y + 1.0, r.w, h - 1.0));
        c.set_clip(Some(prev));
    }

    // --- Navigateur ---

    /// Tout ce que propose le navigateur : (catégorie, nom, description, contenu).
    fn browser_entries(&self) -> Vec<BrowserEntry> {
        let mut v = Vec::new();
        for k in registry::instruments() {
            let payload = Payload::Instrument { kind: k.id.into(), name: tr(k.name).into() };
            v.push((0, tr(k.name).to_string(), tr(k.description).to_string(), payload));
        }
        for k in registry::effects() {
            let payload = Payload::Effect { kind: k.id.into(), name: tr(k.name).into() };
            v.push((1, tr(k.name).to_string(), tr(k.description).to_string(), payload));
        }
        for p in &self.sample_files {
            let name = p.file_stem().map_or(String::new(), |s| s.to_string_lossy().into_owned());
            let folder = match compat::library_folder(p) {
                Some(f) => tr_fmt!("Library · {}", f),
                None => p.parent().and_then(|p| p.file_name()).map_or(String::new(), |s| s.to_string_lossy().into_owned()),
            };
            v.push((2, name, folder, Payload::Sample(p.clone())));
        }
        // Instruments then effects; by name, CLAP before VST 3 for the same plugin.
        let clap = self.plugins.iter().map(|p| (0, "CLAP", p.kind(), &p.name, &p.vendor, p.is_instrument));
        let vst3 = self.vst3_plugins.iter().map(|p| (1, "VST 3", p.kind(), &p.name, &p.vendor, p.is_instrument));
        let mut plugins: Vec<_> = clap.chain(vst3).collect();
        plugins.sort_by_cached_key(|p| (!p.5, p.3.to_lowercase(), p.0));
        for (_, format, kind, name, vendor, is_instrument) in plugins {
            let what = if is_instrument { tr("Instrument") } else { tr("Effect") };
            let detail =
                if vendor.is_empty() { format!("{what} {format}") } else { format!("{what} {format} · {vendor}") };
            let payload = if is_instrument {
                Payload::Instrument { kind, name: name.clone() }
            } else {
                Payload::Effect { kind, name: name.clone() }
            };
            v.push((3, name.clone(), detail, payload));
        }
        v
    }

    fn sidebar_area(&mut self, c: &mut Canvas, input: &Input, r: Rect) {
        const CATS: [(Icon, &str); 4] =
            [(Icon::Instrument, tr_noop("Instruments")), (Icon::Effect, tr_noop("Effects")), (Icon::Sample, tr_noop("Samples")), (Icon::Plugin, tr_noop("CLAP and VST 3 plugins"))];
        let t = self.theme;
        c.fill_rect(r, t.bg_deep);
        c.fill_rect(Rect::new(r.right() - 1.0, r.y, 1.0, r.h), t.hairline);
        let prev = c.push_clip(r);
        let full = self.layout.sidebar_open.max(r.w);
        let inner_x = r.right() - full + 12.0;
        let w = full - 24.0;

        // Recherche : filtre instruments, effets et échantillons.
        let field = Rect::new(inner_x, r.y + 12.0, w, 36.0);
        let fid = wid("sb-search", 0, 0);
        let fhov = self.hovered(input, fid, field);
        if fhov {
            self.set_cursor(ponzik_window::Cursor::Text);
        }
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
        draw_icon(c, Icon::Search, field.x + 18.0, field.y + 18.0, 14.0, t.muted);
        let query = self.sidebar_query.clone();
        if query.is_empty() && !self.sidebar_focus {
            self.text(c, self.f.sans, 13.0, field.x + 34.0, field.y + 18.0, tr("Search"), t.muted);
            if fhv > 0.01 {
                let key = self.key_label(super::keymap::Shortcut::Search);
                if !key.is_empty() {
                    self.keycap(c, field.right() - 8.0, field.y + 18.0, &key, fhv);
                }
            }
        }
        let tw = self.text(c, self.f.sans, 13.0, field.x + 34.0, field.y + 18.0, &query, t.text);
        if self.sidebar_focus {
            if (self.time * 2.0).fract() < 0.6 {
                c.fill_rect(Rect::new(field.x + 35.0 + if query.is_empty() { 0.0 } else { tw }, field.y + 9.0, 1.5, 18.0), t.select);
            }
            self.anim.set(wid("caret", 0, 0), self.time);
        }

        // Catégories : des icônes ; la recherche les parcourt toutes.
        let searching = !query.trim().is_empty();
        let cats = Rect::new(inner_x, field.bottom() + 10.0, w, 36.0);
        let icons: Vec<Icon> = CATS.iter().map(|c| c.0).collect();
        let tips: Vec<(&str, &str)> = CATS.iter().map(|c| (tr(c.1), "")).collect();
        if let Some(i) = self.icon_segmented(c, input, wid("sb-cats", 0, 0), cats, &icons, &tips, self.sidebar_tab) {
            self.sidebar_tab = i;
            self.sidebar_scroll = 0.0;
            self.sidebar_query.clear();
        }

        let list = Rect::new(r.x, cats.bottom() + 6.0, r.w, r.bottom() - 8.0 - cats.bottom() - 6.0);
        let hov_list = input.inside && list.contains(input.x, input.y);
        if hov_list && input.wheel != 0.0 {
            self.sidebar_scroll = (self.sidebar_scroll - input.wheel * 60.0).max(0.0);
        }
        let scroll = self.anim.to(wid("sb-scroll", 0, 0), self.sidebar_scroll, T_LAYOUT);
        let inner = c.push_clip(list);
        let mut y = list.y + 6.0 - scroll;

        // Filtre et tri par pertinence.
        let key = (
            self.sample_files.as_ptr() as usize,
            self.sample_files.len(),
            self.plugins.len() + self.vst3_plugins.len(),
            ponzik_i18n::lang().code(),
        );
        if self.browser_cache.0 != Some(key) {
            self.browser_cache = (Some(key), self.browser_entries());
        }
        let all = std::mem::take(&mut self.browser_cache.1);
        let mut entries: Vec<&BrowserEntry> = Vec::new();
        if searching {
            let mut scored: Vec<(i32, &BrowserEntry)> = all
                .iter()
                .filter_map(|e| super::palette::fuzzy_score(&query, &format!("{} {}", e.1, e.2)).map(|s| (s, e)))
                .collect();
            scored.sort_by_key(|s| (s.1.0, std::cmp::Reverse(s.0)));
            entries.extend(scored.into_iter().map(|(_, e)| e));
        } else {
            entries.extend(all.iter().filter(|e| e.0 == self.sidebar_tab));
        }
        if enter && let Some(e) = entries.first() {
            self.add_default(e.3.clone());
        }

        let mut last_cat = usize::MAX;
        let mut last_group = "";
        let lib_prefix = tr_fmt!("Library · {}", "");
        for (k, (cat, name, detail, payload)) in entries.iter().map(|e| (&e.0, &e.1, &e.2, &e.3)).enumerate() {
            // Samples: the library first, one heading per folder, then the rest;
            // plugins: instruments, then effects.
            let group = match (cat, payload) {
                (2, _) => detail.strip_prefix(lib_prefix.as_str()).unwrap_or(tr("Your samples")),
                (3, Payload::Instrument { .. }) => tr("Instruments"),
                (3, _) => tr("Effects"),
                _ => "",
            };
            if !searching && group != last_group {
                y += if last_group.is_empty() { 4.0 } else { 10.0 };
                self.text(c, self.f.sans_med, 11.5, inner_x + 12.0, y + 8.0, group, t.muted);
                y += 20.0;
                last_group = group;
            }
            if searching && *cat != last_cat {
                // Section : son icône et un filet.
                y += if last_cat == usize::MAX { 4.0 } else { 12.0 };
                draw_icon(c, CATS[*cat].0, inner_x + 12.0, y + 8.0, 13.0, t.muted);
                c.fill_rect(Rect::new(inner_x + 26.0, y + 8.0, w - 30.0, 1.0), t.hairline);
                y += 20.0;
                last_cat = *cat;
            }
            let row = Rect::new(inner_x, y, w, 34.0);
            let badge = match payload {
                Payload::Instrument { kind, .. } | Payload::Effect { kind, .. } => plugin_badge(kind),
                Payload::Sample(_) => None,
            };
            if row.bottom() > list.y && row.y < list.bottom() && self.browser_row(c, input, wid("br", *cat, k), row, name, detail, badge) {
                self.begin_dnd(input, payload.clone());
            }
            y += 36.0;
        }
        let plugins_tab = !searching && self.sidebar_tab == 3;
        if entries.is_empty() && (!plugins_tab || self.scan_errors.is_empty()) {
            let (icon, line) = match (searching, self.sidebar_tab) {
                (true, _) => (Icon::Search, tr("Nothing found")),
                (_, 2) => (Icon::Sample, tr("No WAV files")),
                (_, 3) if self.plugin_scan_running() => (Icon::Plugin, tr("Scanning the plugins…")),
                (_, 3) => (Icon::Plugin, tr("No CLAP or VST 3 plugins")),
                _ => (Icon::Info, tr("Empty")),
            };
            self.empty_state(c, Rect::new(inner_x, list.y + 20.0, w, 120.0), icon, line);
            y = y.max(list.y + 150.0 - scroll);
        }
        if plugins_tab {
            y = self.plugin_scan_rows(c, input, list, inner_x, w, y);
        }
        drop(entries);
        self.browser_cache.1 = all;
        let content_h = y + scroll - list.y + 16.0;
        self.sidebar_scroll = self.sidebar_scroll.min((content_h - list.h).max(0.0));
        c.set_clip(Some(inner));
        c.set_clip(Some(prev));
    }

    /// Ligne du navigateur : le nom seul, la description en infobulle. Renvoie
    /// vrai à la saisie (clic : ajout, glisser : dépôt où l'on veut). `badge` :
    /// format d'un plugin (CLAP, VST3), en pastille à droite.
    #[allow(clippy::too_many_arguments)]
    fn browser_row(&mut self, c: &mut Canvas, input: &Input, id: u64, row: Rect, title: &str, detail: &str, badge: Option<&str>) -> bool {
        let t = self.theme;
        let hov = self.hovered(input, id, row) && !self.dnd_active();
        let hv = self.hover_anim(id, hov);
        if hv > 0.01 {
            c.fill_rrect(row, 8.0, t.raised.alpha(hv));
        }
        let mut title_w = row.w - 40.0;
        if let Some(badge) = badge {
            let bw = self.fonts.measure(self.f.mono_med, 9.0, badge) + 10.0;
            let pill = Rect::new(row.right() - 30.0 - bw, row.y + row.h * 0.5 - 8.0, bw, 16.0);
            // CLAP in the accent, VST 3 in the select tint: told apart at a glance.
            let tint = if badge == "CLAP" { t.accent } else { t.select };
            c.fill_rrect(pill, 8.0, tint.alpha(0.14 + 0.1 * hv));
            c.stroke_rrect(pill, 8.0, 1.0, tint.alpha(0.45));
            let (px, py) = pill.center();
            self.text_center(c, self.f.mono_med, 9.0, px, py, badge, tint);
            title_w -= bw + 8.0;
        }
        self.text_fit(c, self.f.sans_med, 13.0, row.x + 12.0, row.y + row.h * 0.5, title, title_w, t.text2.mix(t.text, hv));
        if hv > 0.01 {
            draw_icon(c, Icon::Plus, row.right() - 16.0, row.y + row.h * 0.5, 13.0, t.accent.alpha(hv));
        }
        if hov {
            let tip = if detail.is_empty() { tr("Click: add · drag: drop").to_string() } else { tr_fmt!("{}\nClick: add · drag: drop", detail) };
            self.set_tip(&tip, "");
        }
        hov && input.pressed
    }

    // --- Menus ---

    pub fn open_popup(&mut self, kind: Popup, anchor: Rect) {
        self.popup = Some(PopupState { kind, anchor, fresh: true });
        self.anim.set(wid("popup", 0, 0), 0.0);
        self.anim.set(wid("popup-scroll", 0, 0), 0.0);
    }

    /// Lignes d'un menu, sous un titre.
    fn menu_items(&self, kind: &Popup) -> (String, Vec<MenuItem>) {
        let p = &self.project;
        let track_name = |t: usize| p.tracks[t].name.clone();
        let bars = |len: u32| format!("{}", len.div_ceil(TICKS_PER_BAR));
        match kind {
            Popup::AddInstrument { replace } => (
                if replace.is_some() { tr("Replace with") } else { tr("Instruments") }.into(),
                registry::instruments()
                    .iter()
                    .map(|k| MenuItem::new(Icon::Instrument, tr(k.name), Action::AddInstrument(k.id, *replace)).tip(tr(k.description)))
                    .collect(),
            ),
            Popup::AddEffect { track } => (
                track_name(*track),
                registry::effects().iter().map(|k| MenuItem::new(Icon::Effect, tr(k.name), Action::AddEffect(*track, k.id)).tip(tr(k.description))).collect(),
            ),
            Popup::AddModulator { track, channel } => (tr("Modulators").into(), self.add_mod_items(*track, *channel)),
            Popup::RackAdd { track, slot, chain } => (tr_fmt!("Chain {}", chain + 1), self.rack_add_items(*track, *slot, *chain)),
            Popup::RackDevice { track, slot, unit } => {
                let (kind, _) = self.device_kind(super::devices::Device::RackUnit { track: *track, slot: *slot, unit: *unit });
                let name = registry::effect_kind(&kind).map_or(kind.clone(), |k| tr(k.name).to_string());
                (name, self.rack_device_items(*track, *slot, *unit))
            }
            Popup::AddNoteFx { channel, slot } => (
                tr("Note effects").into(),
                registry::note_fx()
                    .iter()
                    .map(|k| MenuItem::new(super::notefx::icon(k.id), tr(k.name), Action::AddNoteFx(*channel, *slot, k.id)).tip(tr(k.description)))
                    .collect(),
            ),
            Popup::Patterns => {
                let mut v: Vec<MenuItem> = p
                    .patterns
                    .iter()
                    .enumerate()
                    .map(|(i, pat)| {
                        let mut it = MenuItem::plain(&pat.name, Action::SelectPattern(i)).detail(bars(pat.len)).on(i == p.current_pattern);
                        it.color = Some(self.pattern_tint(i));
                        it
                    })
                    .collect();
                v.push(MenuItem::new(Icon::Plus, tr("New"), Action::NewPattern).sep());
                v.push(MenuItem::new(Icon::Duplicate, tr("Duplicate"), Action::DuplicatePattern));
                let cur = p.current_pattern;
                let rename = Popup::Rename { target: Rename::Pattern(cur), text: p.patterns[cur].name.clone() };
                v.push(MenuItem::new(Icon::Pencil, tr("Rename"), Action::Popup(rename)));
                if p.patterns.len() > 1 {
                    v.push(MenuItem::new(Icon::Trash, tr("Delete"), Action::DeletePattern).tip(tr("Also removes its clips")));
                }
                (tr("Patterns").into(), v)
            }
            Popup::OpenProject(files) => {
                let items = if files.is_empty() {
                    vec![MenuItem::new(Icon::Folder, tr("No projects"), Action::None).tip(&compat::projects_dir().display().to_string())]
                } else {
                    files
                        .iter()
                        .map(|f| {
                            let name = f.file_stem().map_or(String::new(), |s| s.to_string_lossy().into_owned());
                            let dir = f.parent().map_or(String::new(), |d| d.display().to_string());
                            MenuItem::plain(&name, Action::Open(f.clone())).tip(&dir)
                        })
                        .collect()
                };
                (tr("Projects").into(), items)
            }
            Popup::ChannelMenu(ch) => {
                let ch = *ch;
                let c = &p.channels[ch];
                let rename = Popup::Rename { target: Rename::Channel(ch), text: c.name.clone() };
                (
                    c.name.clone(),
                    vec![
                        MenuItem::new(Icon::Piano, tr("Piano roll"), Action::ChannelPianoRoll(ch)),
                        MenuItem::new(Icon::Pencil, tr("Rename"), Action::Popup(rename)),
                        MenuItem::new(Icon::Swap, tr("Replace"), Action::Popup(Popup::AddInstrument { replace: Some(ch) })),
                        MenuItem::new(Icon::Duplicate, tr("Duplicate"), Action::DuplicateChannel(ch)).tip(tr("Same settings, without the notes")),
                        MenuItem::new(Icon::Trash, tr("Delete"), Action::DeleteChannel(ch)).tip(tr("Also removes its notes")),
                    ],
                )
            }
            Popup::TrackMenu(t) => {
                let t = *t;
                let trk = &p.tracks[t];
                let rename = Popup::Rename { target: Rename::Track(t), text: trk.name.clone() };
                let mut v = vec![MenuItem::new(Icon::Pencil, tr("Rename"), Action::Popup(rename))];
                let vca = trk.kind == TrackKind::Vca;
                if t != MASTER && !vca {
                    v.push(MenuItem::new(Icon::Route, tr("Output"), Action::Popup(Popup::Output(t))).detail(track_name(trk.output)));
                    v.push(MenuItem::new(Icon::Send, tr("Send"), Action::Popup(Popup::Send(t))));
                }
                if !vca {
                    v.push(MenuItem::new(Icon::Effect, tr("Effect"), Action::Popup(Popup::AddEffect { track: t })));
                }
                if t != MASTER && !vca {
                    // Grouper : la sélection si la piste en fait partie, sinon la piste seule.
                    let group = self.group_for(t);
                    let item = if group.len() > 1 {
                        MenuItem::new(Icon::Bus, tr("Group into a bus"), Action::NewBus(group.clone()))
                            .detail("Ctrl+G".into())
                            .tip(&tr_fmt!("The {} selected tracks go out into a new bus", group.len()))
                    } else {
                        MenuItem::new(Icon::Bus, tr("Route to a new bus"), Action::NewBus(group.clone()))
                            .detail("Ctrl+G".into())
                            .tip(tr("Creates a bus and routes this track into it"))
                    };
                    v.push(item.sep());
                    v.push(
                        MenuItem::new(Icon::Send, tr("New send bus"), Action::Popup(Popup::NewReturn(t)))
                            .tip(tr("A return bus with a reverb or a delay, and a send to it")),
                    );
                    let detail = trk.vca.map_or(String::new(), track_name);
                    v.push(MenuItem::new(Icon::Vca, tr("VCA"), Action::Popup(Popup::Vca(t))).detail(detail).tip(tr("A fader that controls the volume of several tracks")));
                }
                match trk.kind {
                    TrackKind::Bus => {
                        let members = p.bus_members(t).len();
                        if members > 0 {
                            let (icon, name) = if trk.folded { (Icon::ChevronRight, tr("Unfold")) } else { (Icon::ChevronLeft, tr("Fold")) };
                            v.push(MenuItem::new(icon, name, Action::ToggleFold(t)).detail(members.to_string()).sep());
                        }
                        let it = MenuItem::new(Icon::Close, tr("Ungroup the bus"), Action::Dissolve(t)).tip(tr("Its tracks go out where it went out"));
                        v.push(if members > 0 { it } else { it.sep() });
                    }
                    TrackKind::Vca => {
                        v.push(MenuItem::new(Icon::Close, tr("Delete the VCA"), Action::Dissolve(t)).tip(tr("Its tracks keep their own volume")).sep());
                    }
                    TrackKind::Normal => {}
                }
                v.push(MenuItem::new(Icon::Reset, tr("Reset"), Action::ResetTrack(t)).tip(tr("Volume 0 dB, pan centered")));
                (trk.name.clone(), v)
            }
            Popup::Output(t) | Popup::Send(t) => {
                let t = *t;
                let send = matches!(kind, Popup::Send(_));
                let mut v: Vec<MenuItem> = (0..TRACKS)
                    .filter(|&d| d != t && p.tracks[d].kind != TrackKind::Vca && self.routing_ok(t, d, send))
                    .filter(|&d| d == MASTER || self.track_in_use(d))
                    .map(|d| {
                        let action = if send { Action::AddSend(t, d) } else { Action::SetOutput(t, d) };
                        let mut it = MenuItem::plain(&track_name(d), action)
                            .detail(if d == MASTER { "M".into() } else { d.to_string() })
                            .on(!send && p.tracks[t].output == d);
                        if p.tracks[d].kind == TrackKind::Bus {
                            it.color = Some(self.track_tint(d));
                        }
                        it
                    })
                    .collect();
                let new = if send {
                    MenuItem::new(Icon::Plus, tr("New send bus"), Action::Popup(Popup::NewReturn(t)))
                } else {
                    MenuItem::new(Icon::Plus, tr("New bus"), Action::NewBus(vec![t]))
                };
                v.push(new.sep());
                (if send { tr("Send to") } else { tr("Output to") }.into(), v)
            }
            Popup::NewBus => {
                let sel = self.mixer_sel.clone();
                let n = sel.len();
                let group = if n > 0 {
                    MenuItem::new(Icon::Bus, tr("Group the selection"), Action::NewBus(sel.clone()))
                        .detail(format!("{n} · Ctrl+G"))
                        .tip(tr("The selected tracks (Ctrl+click on their name) go out into a new bus"))
                } else {
                    MenuItem::new(Icon::Bus, tr("Bus"), Action::NewBus(Vec::new())).tip(tr("An empty bus; Ctrl+click strips then Ctrl+G to group them"))
                };
                let mut v = vec![group];
                for (kind, name, _) in ponzik_project::RETURN_EFFECTS {
                    v.push(MenuItem::new(Icon::Send, &tr_fmt!("Return with {}", tr(name).to_lowercase()), Action::NewReturn(kind, None)).tip(tr("Return bus, effect 100% wet")));
                }
                let vca = MenuItem::new(Icon::Vca, tr("VCA"), Action::NewVca(sel)).tip(tr("A fader that controls the volume of several tracks without carrying their sound"));
                v.push(if n > 0 { vca.detail(n.to_string()).sep() } else { vca.sep() });
                (tr("New").into(), v)
            }
            Popup::NewReturn(t) => {
                let t = *t;
                let v = ponzik_project::RETURN_EFFECTS
                    .iter()
                    .map(|&(kind, name, _)| {
                        MenuItem::new(Icon::Effect, tr(name), Action::NewReturn(kind, Some(t))).tip(tr("New return bus, send at −10 dB from this track"))
                    })
                    .collect();
                (tr("Send bus").into(), v)
            }
            Popup::Vca(t) => {
                let t = *t;
                let current = p.tracks[t].vca;
                let mut v = vec![MenuItem::new(Icon::Close, tr("None"), Action::SetVca(t, None)).on(current.is_none())];
                for d in (1..TRACKS).filter(|&d| p.tracks[d].kind == TrackKind::Vca) {
                    let mut it = MenuItem::plain(&track_name(d), Action::SetVca(t, Some(d))).detail(d.to_string()).on(current == Some(d));
                    it.color = Some(self.track_tint(d));
                    v.push(it);
                }
                let group = self.group_for(t);
                v.push(MenuItem::new(Icon::Plus, tr("New VCA"), Action::NewVca(group)).sep());
                (tr("VCA").into(), v)
            }
            Popup::Sidechain { track, slot } => {
                let (t, slot) = (*track, *slot);
                let current = p.tracks[t].inserts[slot].as_ref().and_then(|d| d.sidechain);
                let none = MenuItem::new(Icon::Close, tr_ctx("sidechain", "None"), Action::SetSidechain(t, slot, None));
                let mut v = vec![none.on(current.is_none())];
                let audible = |d: usize| p.tracks[d].kind != TrackKind::Vca;
                for d in (1..TRACKS).filter(|&d| d != t && audible(d) && (self.track_in_use(d) || current == Some(d))) {
                    let ok = self.sidechain_ok(t, slot, d);
                    let detail = if ok { d.to_string() } else { tr("loop").into() };
                    let mut it = MenuItem::plain(&track_name(d), Action::SetSidechain(t, slot, Some(d)))
                        .detail(detail)
                        .on(current == Some(d))
                        .dim(!ok);
                    it.color = Some(self.track_tint(d).alpha(if ok { 1.0 } else { 0.3 }));
                    if !ok {
                        it = it.tip(tr("Not possible: would create a routing loop"));
                    }
                    v.push(it);
                }
                (tr("Sidechain track").into(), v)
            }
            Popup::Param { target, value, default } => {
                let label = self.target_info(*target).map_or(String::new(), |(l, _)| l);
                let mut v = vec![MenuItem::new(Icon::Automation, tr("Automate"), Action::Automate(*target, *value)).tip(tr("A curve in the playlist"))];
                if self.is_automated(*target) {
                    v.push(MenuItem::new(Icon::Trash, tr("Delete the automation"), Action::RemoveAutomation(*target)));
                }
                v.push(MenuItem::new(Icon::Reset, tr("Default value"), Action::SetParamValue(*target, *default)));
                v.extend(self.macro_menu_items(*target));
                v.extend(self.mod_menu_items(*target));
                (label, v)
            }
            Popup::ImportMidi(files) => {
                let items = if files.is_empty() {
                    vec![MenuItem::new(Icon::Piano, tr("No MIDI files"), Action::None).tip(&compat::projects_dir().display().to_string())]
                } else {
                    files
                        .iter()
                        .map(|f| {
                            let name = f.file_stem().map_or(String::new(), |s| s.to_string_lossy().into_owned());
                            let dir = f.parent().map_or(String::new(), |d| d.display().to_string());
                            MenuItem::new(Icon::Piano, &name, Action::ImportMidi(f.clone())).tip(&dir)
                        })
                        .collect()
                };
                (tr("Import MIDI").into(), items)
            }
            Popup::ImportFlp(files) => {
                let items = if files.is_empty() {
                    vec![MenuItem::new(Icon::Folder, tr("No FL Studio project found"), Action::None)
                        .tip(&compat::projects_dir().display().to_string())]
                } else {
                    files
                        .iter()
                        .map(|f| {
                            let name = f.file_stem().map_or(String::new(), |s| s.to_string_lossy().into_owned());
                            let dir = f.parent().map_or(String::new(), |d| d.display().to_string());
                            MenuItem::new(Icon::Folder, &name, Action::ImportFlp(f.clone())).tip(&dir)
                        })
                        .collect()
                };
                (tr("Import an FL Studio project").into(), items)
            }
            Popup::ImportReport => {
                let items = if self.import_report.is_empty() {
                    vec![MenuItem::plain(tr("No FL Studio import yet"), Action::None)]
                } else {
                    let rows = self.import_report.iter().enumerate();
                    rows.map(|(i, (text, more))| {
                        let row = MenuItem::plain(text, Action::None).dim(*more);
                        if !more && i > 0 { row.sep() } else { row }
                    })
                    .collect()
                };
                (tr("FL Studio import report").into(), items)
            }
            Popup::Presets { device, kind } => {
                let save = Popup::Rename { target: Rename::Preset(*device), text: String::new() };
                let mut v = vec![MenuItem::new(Icon::Save, tr("Save…"), Action::Popup(save)).tip(tr("The current settings, under a name"))];
                for (i, path) in compat::list_presets(kind).into_iter().enumerate() {
                    let name = path.file_stem().map_or(String::new(), |s| s.to_string_lossy().into_owned());
                    let it = MenuItem::new(Icon::Preset, &name, Action::LoadPreset(*device, path));
                    v.push(if i == 0 { it.sep() } else { it });
                }
                for (i, name) in ponzik_project::presets::factory_preset_names(kind).into_iter().enumerate() {
                    let it = MenuItem::new(Icon::Preset, name, Action::LoadFactoryPreset(*device, name)).detail(tr("Factory").into());
                    v.push(if i == 0 { it.sep() } else { it });
                }
                (tr("Presets").into(), v)
            }
            Popup::SessionAssign { col, row } => (
                tr_fmt!("Track {} · scene {}", col + 1, row + 1),
                p.patterns
                    .iter()
                    .enumerate()
                    .map(|(i, pat)| {
                        let mut it = MenuItem::plain(&pat.name, Action::AssignSession(*col, *row, i)).detail(bars(pat.len));
                        it.color = Some(self.pattern_tint(i));
                        it
                    })
                    .collect(),
            ),
            Popup::Templates => (
                tr("New project").into(),
                ponzik_project::templates()
                    .into_iter()
                    .enumerate()
                    .map(|(i, (name, make))| {
                        let p = make();
                        let bpm = format!("{:.0}", p.bpm);
                        let it = MenuItem::new(template_icon(i), tr(name), Action::NewFromTemplate(i)).detail(bpm);
                        if i == 1 { it.sep() } else { it }
                    })
                    .collect(),
            ),
            Popup::History => {
                // Le plus récent en haut : les étapes annulées (en retrait), puis celles
                // qui ont mené à l'état actuel, jusqu'au départ.
                let (past, future) = (self.past.len(), self.future.len());
                let mark = |it: MenuItem, col: Color| MenuItem { color: Some(col), ..it };
                let mut v: Vec<MenuItem> = self
                    .future
                    .iter()
                    .enumerate()
                    .map(|(i, (label, _))| mark(MenuItem::plain(label, Action::JumpHistory(past + future - i)).dim(true), self.theme.raised_hi))
                    .collect();
                v.extend(self.past.iter().enumerate().rev().map(|(j, (label, _))| {
                    mark(MenuItem::plain(label, Action::JumpHistory(j + 1)).on(j + 1 == past), self.theme.accent.alpha(0.7))
                }));
                v.push(mark(MenuItem::plain(tr("Start"), Action::JumpHistory(0)).on(past == 0).dim(past > 0), self.theme.muted).sep());
                (tr("History").into(), v)
            }
            Popup::Humanize => (
                tr("Humanize").into(),
                super::piano::HUMANIZE
                    .iter()
                    .map(|&(name, ticks, vel)| {
                        MenuItem::new(Icon::Dice, tr(name), Action::Humanize(ticks, vel))
                            .detail(tr_fmt!("±{} · ±{:.0}%", ticks, vel * 100.0))
                            .tip(tr("Random start (ticks) and velocity"))
                    })
                    .collect(),
            ),
            Popup::MainMenu => {
                use super::keymap::Shortcut as S;
                let key = |s: S| self.key_label(s);
                let theme = THEMES.iter().position(|th| th.name == self.theme.name).unwrap_or(0);
                (
                    "Ponzik".into(),
                    vec![
                        MenuItem::new(Icon::Plus, tr("New"), Action::NewProject).detail(key(S::NewProject)),
                        MenuItem::new(Icon::Folder, tr("Open"), Action::OpenMenu).detail(key(S::Open)),
                        MenuItem::new(Icon::Save, tr("Save"), Action::Save).detail(key(S::Save)),
                        MenuItem::new(Icon::Pencil, tr("Save as"), Action::SaveAs).detail(key(S::SaveAs)),
                        MenuItem::new(Icon::Export, tr("Export as WAV"), Action::Export).detail(key(S::Export)).sep(),
                        MenuItem::new(Icon::Columns, tr("Export stems"), Action::ExportStems).tip(tr("One WAV per mixer track")),
                        MenuItem::new(Icon::Columns, tr("Export sections"), Action::ExportSections(Default::default()))
                            .tip(tr("One WAV per arrangement section: \"<project> - <section>.wav\"")),
                        MenuItem::new(Icon::Piano, tr("Import MIDI"), Action::ImportMidiMenu).sep(),
                        MenuItem::new(Icon::Folder, tr("Import an FL Studio project"), Action::ImportFlpMenu),
                        MenuItem::new(Icon::Piano, tr("Export as MIDI"), Action::ExportMidi).tip(tr("The current pattern")),
                        MenuItem::new(Icon::Play, tr("House demo"), Action::LoadDemo(0)).sep(),
                        MenuItem::new(Icon::Play, tr("Breakcore demo"), Action::LoadDemo(1)),
                        MenuItem::new(Icon::Play, tr("Zalgo rave demo"), Action::LoadDemo(2)),
                        MenuItem::new(Icon::Theme, tr("Theme"), Action::Popup(Popup::Themes)).detail(tr(THEMES[theme].label).into()).sep(),
                        MenuItem::new(Icon::Layout, tr("Settings"), Action::Popup(Popup::Settings { tab: 0 }))
                            .detail(key(S::Settings))
                            .tip(tr("Views, panels and shortcuts")),
                    ],
                )
            }
            Popup::Themes => (
                tr("Theme").into(),
                THEMES
                    .iter()
                    .enumerate()
                    .map(|(i, th)| {
                        let mut it = MenuItem::plain(tr(th.label), Action::SetTheme(i)).on(th.name == self.theme.name);
                        it.color = Some(th.accent);
                        it
                    })
                    .collect(),
            ),
            Popup::ClipMenu(i) => {
                let Some(clip) = p.clips.get(*i) else { return (String::new(), Vec::new()) };
                let pat = clip.pattern;
                let shared = p.clips.iter().filter(|c| c.pattern == pat).count();
                let rename = Popup::Rename { target: Rename::Pattern(pat), text: p.patterns[pat].name.clone() };
                let unique = MenuItem::new(Icon::Duplicate, tr("Make unique"), Action::MakeUnique(*i));
                let unique = if shared > 1 {
                    unique.detail(tr_fmt!("{} clips", shared)).tip(tr("Copy of the pattern for this clip alone"))
                } else {
                    unique.tip(tr("This clip is already the only one playing this pattern"))
                };
                let mut color = MenuItem::plain(tr("Color"), Action::Popup(Popup::PatternColor(pat)));
                color.color = Some(self.pattern_tint(pat));
                (
                    p.patterns[pat].name.clone(),
                    vec![
                        unique,
                        MenuItem::new(Icon::Pencil, tr("Rename"), Action::Popup(rename)).tip(tr("Renames the pattern and all its clips")),
                        color,
                        MenuItem::new(Icon::Rack, tr("Edit"), Action::EditPattern(pat)).detail(tr("double-click").into()),
                        MenuItem::new(Icon::Trash, tr("Delete the clip"), Action::DeleteClip(*i)).tip(tr("Hold right-click: sweep to erase")),
                    ],
                )
            }
            Popup::PatternMenu(pat) => {
                let pat = *pat;
                let rename = Popup::Rename { target: Rename::Pattern(pat), text: p.patterns[pat].name.clone() };
                let mut color = MenuItem::plain(tr("Color"), Action::Popup(Popup::PatternColor(pat)));
                color.color = Some(self.pattern_tint(pat));
                let mut v = vec![
                    MenuItem::new(Icon::Rack, tr("Edit"), Action::EditPattern(pat)).detail(self.key_label(super::keymap::Shortcut::ViewRack)),
                    MenuItem::new(Icon::Pencil, tr("Rename"), Action::Popup(rename)),
                    color,
                    MenuItem::new(Icon::Duplicate, tr("Duplicate"), Action::DuplicatePattern),
                ];
                if p.patterns.len() > 1 {
                    v.push(MenuItem::new(Icon::Trash, tr("Delete"), Action::DeletePattern).tip(tr("Also removes its clips")));
                }
                (p.patterns[pat].name.clone(), v)
            }
            Popup::PatternColor(pat) => {
                let pat = *pat;
                let cur = p.patterns[pat].color;
                let mut auto = MenuItem::plain(tr("Automatic"), Action::SetPatternColor(pat, 0)).on(cur == 0);
                auto.color = Some(super::playlist::pattern_color(pat));
                let mut v = vec![auto];
                for (i, &(name, hex)) in super::playlist::PATTERN_COLORS.iter().enumerate() {
                    let mut it = MenuItem::plain(tr(name), Action::SetPatternColor(pat, hex)).on(cur == hex);
                    it.color = Some(Color::hex(hex));
                    v.push(if i == 0 { it.sep() } else { it });
                }
                (tr_fmt!("Color · {}", p.patterns[pat].name.as_str()), v)
            }
            Popup::PadMenu { channel, pad } => self.pad_menu(*channel, *pad),
            Popup::MarkerMenu(i) => self.marker_menu(*i),
            Popup::MarkerColor(i) => self.marker_color_menu(*i),
            Popup::Signature(tick) => self.signature_menu(*tick),
            Popup::Rename { .. } | Popup::Palette { .. } | Popup::Scale | Popup::Chord | Popup::Settings { .. } => (String::new(), Vec::new()),
        }
    }

    pub fn track_in_use(&self, t: usize) -> bool {
        self.project.track_in_use(t)
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

    /// Vrai si la piste `key` peut servir de clé à l'insert `slot` de `track` sans boucle.
    pub fn sidechain_ok(&self, track: usize, slot: usize, key: usize) -> bool {
        let mut p = self.project.clone();
        match p.tracks[track].inserts[slot].as_mut() {
            Some(fx) => fx.sidechain = Some(key),
            None => return false,
        }
        p.routing_order().is_some()
    }

    pub(super) fn run_action(&mut self, action: Action, anchor: Rect) {
        match action {
            Action::None => {}
            Action::Rack(a) => self.run_rack_action(a),
            Action::Mod(a) => self.run_mod_action(a),
            Action::AddInstrument(kind, None) => self.add_instrument(kind),
            Action::AddInstrument(kind, Some(ch)) => {
                if let Some(info) = registry::instrument_kind(kind) {
                    let c = &mut self.project.channels[ch];
                    c.instrument = InstrumentDef::new(kind);
                    // Nom par défaut, dans n'importe quelle langue (anciens projets en français).
                    let default_name = |k: &registry::InstrumentKind| ponzik_i18n::Lang::all().any(|l| ponzik_i18n::with_lang(l, || tr(k.name)) == c.name);
                    if c.name.is_empty() || registry::instruments().iter().any(default_name) {
                        c.name = tr(info.name).to_string();
                    }
                    self.commit(tr("Replace the instrument"));
                }
            }
            Action::AddEffect(track, kind) => self.add_effect(track, kind),
            Action::AddNoteFx(channel, slot, kind) => self.add_note_fx(channel, slot, kind),
            Action::SelectPattern(i) => self.select_pattern(i),
            Action::MakeUnique(clip) => self.make_unique(clip),
            Action::SetPatternColor(pat, color) => {
                if let Some(p) = self.project.patterns.get_mut(pat) {
                    p.color = color;
                    self.commit(tr("Pattern color"));
                }
            }
            Action::EditPattern(pat) => {
                self.select_pattern(pat);
                self.set_view(View::Rack);
            }
            Action::DeleteClip(clip) => {
                if clip < self.project.clips.len() {
                    self.project.clips.remove(clip);
                    self.playlist.selected = None;
                    self.commit(tr("Erase a clip"));
                }
            }
            Action::DuplicateTime => self.duplicate_time(),
            Action::NewBus(members) => self.new_bus(&members),
            Action::NewReturn(kind, from) => self.new_return(kind, from),
            Action::NewVca(members) => self.new_vca(&members),
            Action::SetVca(t, vca) => {
                self.project.tracks[t].vca = vca;
                self.commit(if vca.is_some() { tr("Control with a VCA") } else { tr("Remove from the VCA") });
            }
            Action::ToggleFold(t) => self.toggle_fold(t),
            Action::Dissolve(t) => self.dissolve_track(t),
            Action::NewPattern => {
                let p = self.project.add_pattern();
                self.select_pattern(p);
            }
            Action::DuplicatePattern => {
                let mut copy = self.project.patterns[self.project.current_pattern].clone();
                copy.name = tr_fmt!("{} (copy)", copy.name.as_str());
                self.project.patterns.push(copy);
                let p = self.project.patterns.len() - 1;
                self.select_pattern(p);
            }
            Action::DeletePattern => self.delete_pattern(self.project.current_pattern),
            Action::Open(path) => self.open_file(path),
            Action::Popup(kind) => self.open_popup(kind, anchor),
            Action::DuplicateChannel(ch) => {
                let src = self.project.channels[ch].clone();
                let mut instrument = src.instrument.clone();
                // A plugin's copy starts from the patch it has now.
                let editor = self.editors.editor(super::devices::Device::Instrument(ch)).filter(|e| e.is_alive());
                if let Some(state) = editor.and_then(|e| e.save_state(ponzik_engine::StateContext::Duplicate)) {
                    instrument.state = Some(state.into());
                }
                let idx = self.project.add_channel(tr_fmt!("{} (copy)", src.name.as_str()), instrument, src.root_note);
                let c = &mut self.project.channels[idx];
                c.gain = src.gain;
                c.pan = src.pan;
                self.focus = Focus::Channel(idx);
                self.commit(tr("Duplicate a channel"));
            }
            Action::DeleteChannel(ch) => {
                let name = self.project.channels[ch].name.clone();
                self.project.remove_channel(ch);
                self.clamp_selection();
                self.commit(tr("Delete a channel"));
                self.toast(&tr_fmt!("{} deleted · Ctrl+Z to undo", name));
            }
            Action::ChannelPianoRoll(ch) => {
                self.focus = Focus::Channel(ch);
                self.set_view(View::Piano);
            }
            Action::SetOutput(t, d) => {
                self.project.tracks[t].output = d;
                self.commit(tr("Track output"));
            }
            Action::AddSend(t, d) => {
                let sends = &mut self.project.tracks[t].sends;
                match sends.iter().position(|s| s.0 == d && s.1 > 0.0).or_else(|| sends.iter().position(|s| s.1 <= 0.0)) {
                    Some(slot) => {
                        sends[slot] = (d, 0.5);
                        self.commit(tr("Add a send"));
                    }
                    None => self.toast(tr("No free send left on this track")),
                }
            }
            Action::SetSidechain(t, slot, Some(k)) if !self.sidechain_ok(t, slot, k) => {
                let name = self.project.tracks[k].name.clone();
                self.toast(&tr_fmt!("{} can't be used as a sidechain: routing loop", name));
            }
            Action::SetSidechain(t, slot, key) => {
                if let Some(fx) = self.project.tracks[t].inserts[slot].as_mut() {
                    fx.sidechain = key;
                    self.commit(if key.is_some() { tr("Connect a sidechain") } else { tr("Disconnect the sidechain") });
                }
            }
            Action::ResetTrack(t) => {
                self.project.tracks[t].gain = 1.0;
                self.project.tracks[t].pan = 0.0;
                self.commit(tr("Reset a track"));
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
            Action::ToggleCountIn => self.toggle_count_in(),
            Action::ToggleMidiClock(port) => self.toggle_midi_clock(port),
            Action::TapTempo => self.tap_tempo(),
            Action::StartTour => self.start_tour(),
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
            Action::NewProject => self.open_popup(Popup::Templates, anchor),
            Action::NewFromTemplate(i) => {
                if let Some((name, make)) = ponzik_project::templates().get(i).copied() {
                    self.replace_project(make(), None);
                    self.set_view(View::Rack);
                    self.toast(&tr_fmt!("New project · {}", tr(name)));
                }
            }
            Action::Export => self.export(),
            Action::ExportWith(options) => self.export_with(options),
            Action::Undo => self.undo(),
            Action::Redo => self.redo(),
            Action::LoadDemo(i) => {
                let p = match i {
                    1 => ponzik_project::demo_breakcore(),
                    2 => ponzik_project::demo_zalgo(),
                    _ => ponzik_project::demo_project(),
                };
                let name = p.name.clone();
                self.replace_project(p, None);
                self.toast(&tr_fmt!("Demo loaded: {}", name));
            }
            Action::SetTheme(i) => {
                self.theme = THEMES[i.min(THEMES.len() - 1)];
                self.save_settings();
            }
            Action::ApplyPreset(i) => self.apply_profile(i),
            Action::SetLanguage(lang) => self.set_language(lang),
            Action::ToggleViewShown(v) => self.toggle_view_shown(v),
            Action::Pad(channel, pad, op) => self.pad_action(channel, pad, op),
            Action::RescanPlugins => self.start_plugin_scan(true),
            Action::ToggleEditor(device) => self.toggle_editor(device),
            Action::Automate(target, value) => self.create_automation(target, value),
            Action::SaveAs => {
                let text = self.project.name.clone();
                self.open_popup(Popup::Rename { target: Rename::SaveAs, text }, anchor);
            }
            Action::ExportStems => self.export_stems(),
            Action::ExportSections(options) => self.export_sections(options),
            Action::Marker(i, op) => self.marker_op(i, op),
            Action::AddMarkerHere => {
                let tick = self.project.tempo_map().snap_bar(self.playhead());
                if self.project.markers.iter().any(|m| m.tick == tick) {
                    self.toast(tr("There is already a marker on this bar"));
                } else {
                    self.add_marker_at(tick);
                }
            }
            Action::SetSignature(tick, num, den) => {
                self.project.set_signature(tick, num, den);
                self.project.normalize_tempo();
                self.commit(tr("Time signature"));
            }
            Action::DeleteSignature(tick) => {
                self.project.signatures.retain(|s| s.tick != tick);
                self.commit(tr("Delete time signature"));
            }
            Action::ImportMidiMenu => self.open_popup(Popup::ImportMidi(compat::scan_midi()), anchor),
            Action::ImportFlpMenu => self.open_popup(Popup::ImportFlp(compat::scan_flp()), anchor),
            Action::ImportFlp(path) => self.import_flp(&path),
            Action::ImportMidi(path) => match compat::import_midi(&mut self.project, &path) {
                Ok(p) => {
                    self.select_pattern(p);
                    self.commit(tr("Import a MIDI file"));
                    self.set_view(View::Rack);
                    self.toast(&tr_fmt!("Imported into the pattern \"{}\"", self.project.patterns[p].name));
                }
                Err(e) => self.toast(&e),
            },
            Action::ExportMidi => match compat::export_midi(&self.project, self.project.current_pattern) {
                Ok(path) => self.toast(&tr_fmt!("Exported: {}", path.display().to_string())),
                Err(e) => self.toast(&e),
            },
            Action::LoadPreset(device, path) => match compat::load_preset(&path) {
                Ok(preset) => {
                    match preset.state {
                        Some(state) => self.apply_plugin_preset(device, &preset.params, state),
                        None => self.apply_preset(device, &preset.params, preset.rack),
                    }
                    let name = path.file_stem().map_or(String::new(), |s| s.to_string_lossy().into_owned());
                    self.toast(&tr_fmt!("Preset loaded: {}", name));
                }
                Err(e) => self.toast(&e),
            },
            Action::LoadFactoryPreset(device, name) => {
                let (kind, _) = self.device_kind(device);
                if let Some(params) = ponzik_project::presets::factory_preset(&kind, name) {
                    self.apply_preset(device, &params, None);
                    self.toast(&tr_fmt!("Preset loaded: {}", name));
                }
            }
            Action::AssignSession(col, row, p) => {
                self.project.session_grid[col][row] = Some(p);
                self.commit(tr("Fill a slot"));
            }
            Action::Humanize(ticks, vel) => self.humanize(ticks, vel),
            Action::JumpHistory(n) => self.jump_history(n),
            Action::RemoveAutomation(target) => {
                self.project.automation.retain(|a| a.target != target);
                self.commit(tr("Remove an automation"));
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
                self.commit(tr("Default value"));
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
        self.commit(tr("Delete a pattern"));
    }

    fn apply_rename(&mut self, target: Rename, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        match target {
            Rename::Preset(device) => {
                self.save_preset(device, text);
                return;
            }
            Rename::Project => self.project.name = text.into(),
            Rename::SaveAs => {
                self.project.name = text.into();
                self.project_path = None;
                self.commit(tr("Rename"));
                self.save();
                return;
            }
            Rename::Channel(c) => {
                let old = std::mem::replace(&mut self.project.channels[c].name, text.into());
                let t = self.project.channels[c].track;
                if self.project.tracks[t].name == old {
                    self.project.tracks[t].name = text.into();
                }
            }
            Rename::Pattern(p) => self.project.patterns[p].name = text.into(),
            Rename::Track(t) => self.project.tracks[t].name = text.into(),
            Rename::Marker(i) => {
                if let Some(m) = self.project.markers.get_mut(i) {
                    m.name = text.into();
                    self.commit(tr("Rename marker"));
                }
                return;
            }
        }
        self.commit(tr("Rename"));
    }

    fn popup_layer(&mut self, c: &mut Canvas, input: &Input, screen: Rect) {
        let Some(mut state) = self.popup.take() else { return };
        let t = self.theme;
        // Le clic qui vient d'ouvrir le menu est déjà consommé par son bouton.
        let masked;
        let input = if state.fresh {
            masked = Input { pressed: false, right_pressed: false, double: false, ..input.clone() };
            &masked
        } else {
            input
        };
        state.fresh = false;
        if let Popup::Palette { query, selected } = &state.kind {
            let (run, next) = self.palette(c, input, screen, query.clone(), *selected);
            if let Some(kind) = next {
                self.popup = Some(PopupState { kind, anchor: state.anchor, fresh: false });
            }
            if let Some(action) = run {
                self.run_action(action, Rect::new(screen.w * 0.5 - 150.0, screen.h * 0.2, 300.0, 0.0));
            }
            return;
        }
        if matches!(state.kind, Popup::Chord) {
            if self.chord_popup(c, input, state.anchor, screen) {
                self.popup = Some(state);
            }
            return;
        }
        if let Popup::Settings { tab } = state.kind {
            if let Some(tab) = self.settings_sheet(c, input, screen, tab) {
                self.popup = Some(PopupState { kind: Popup::Settings { tab }, ..state });
            }
            return;
        }
        if matches!(state.kind, Popup::Scale) {
            if self.scale_popup(c, input, self.scale_anchor, screen) {
                self.popup = Some(state);
            }
            return;
        }
        let open = self.anim.to(wid("popup", 0, 0), 1.0, T_HOVER);

        // Renommage : un champ texte posé sur le nom (ou au centre, sans ancre).
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
            let a = state.anchor;
            let inline = a.w > 1.0 && a.h > 1.0 && !matches!(target, Rename::SaveAs | Rename::Preset(_));
            let field = if inline {
                let w = a.w.max(200.0);
                let x = a.x.min(screen.right() - w - 8.0).max(8.0);
                Rect::new(x, a.y + a.h * 0.5 - 18.0, w, 36.0)
            } else {
                let w = 360.0;
                let r = Rect::new((screen.w - w) * 0.5, screen.h * 0.32 + (1.0 - open) * 8.0, w, 92.0);
                c.fill_rect(screen, Color::rgba(0, 0, 0, (110.0 * open) as u8));
                c.fill_rrect(r, 16.0, t.panel);
                c.stroke_rrect(r, 16.0, 1.0, t.hairline.alpha(1.6));
                let title = match target {
                    Rename::SaveAs => tr("Save as"),
                    Rename::Preset(_) => tr("Preset name"),
                    _ => tr("Rename"),
                };
                self.text(c, self.f.sans_med, 14.0, r.x + 20.0, r.y + 24.0, title, t.text2);
                Rect::new(r.x + 16.0, r.y + 40.0, r.w - 32.0, 38.0)
            };
            c.fill_rrect(field.translate(0.0, 4.0).inset(-2.0), 12.0, Color::rgba(0, 0, 0, (70.0 * open) as u8));
            c.fill_rrect(field, 10.0, t.well);
            c.stroke_rrect(field, 10.0, 1.5, t.select.alpha(0.9 * open));
            let ok_x = self.keycap(c, field.right() - 8.0, field.y + field.h * 0.5, tr("Enter"), 0.8);
            let shown = self.fit(self.f.sans_med, 14.0, text, ok_x - field.x - 24.0);
            let tw = self.text(c, self.f.sans_med, 14.0, field.x + 12.0, field.y + field.h * 0.5, &shown, t.text);
            if (self.time * 2.0).fract() < 0.6 {
                c.fill_rect(Rect::new(field.x + 13.0 + tw, field.y + 9.0, 1.5, field.h - 18.0), t.select);
            }
            // Tient l'animation éveillée pour faire clignoter le curseur.
            self.anim.set(wid("caret", 0, 0), self.time);
            if input.inside && field.contains(input.x, input.y) {
                self.set_cursor(ponzik_window::Cursor::Text);
            }
            if input.pressed && !field.inset(-16.0).contains(input.x, input.y) {
                // Un clic ailleurs valide, comme dans un champ en place.
                done = Some(inline);
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
        // Assez large pour le plus long libellé et sa valeur (jusqu'à 360 px).
        let widest = items
            .iter()
            .map(|it| self.fonts.measure(self.f.sans_med, 13.0, &it.name) + self.fonts.measure(self.f.mono, 11.0, &it.detail))
            .fold(0.0f32, f32::max);
        let w = (widest + 84.0).clamp(260.0, 360.0);
        let row_h = 38.0;
        let seps = items.iter().filter(|i| i.sep).count() as f32 * 9.0;
        let h = (44.0 + items.len() as f32 * row_h + seps + 6.0).min(screen.h - 80.0);
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
        self.text_fit(c, self.f.sans_med, 12.0, r.x + 16.0, r.y + 20.0, &title, r.w - 32.0, t.muted);

        // Défilement si le menu est plus long que l'écran.
        let list_h = r.h - 40.0;
        let content = items.len() as f32 * row_h + seps;
        let key = wid("popup-scroll", 0, 0);
        let mut scroll = self.anim.get(key).unwrap_or(0.0);
        if input.inside && r.contains(input.x, input.y) && input.wheel != 0.0 {
            scroll -= input.wheel * 60.0;
        }
        scroll = scroll.clamp(0.0, (content - list_h).max(0.0));
        self.anim.set(key, scroll);

        let mut chosen = None;
        let mut hover_tip = None;
        let list = Rect::new(r.x, r.y + 36.0, r.w, r.h - 40.0);
        let prev = c.push_clip(list);
        let has_icons = items.iter().any(|it| it.icon.is_some());
        let mut ry = r.y + 38.0 - scroll;
        for (i, it) in items.iter().enumerate() {
            if it.sep {
                c.fill_rect(Rect::new(r.x + 12.0, ry + 4.0, r.w - 24.0, 1.0), t.hairline);
                ry += 9.0;
            }
            let row = Rect::new(r.x + 6.0, ry, r.w - 12.0, row_h - 4.0);
            ry += row_h;
            let id = wid("popup-row", i, 0);
            let hov = input.inside && row.contains(input.x, input.y) && list.contains(input.x, input.y);
            let hv = self.hover_anim(id, hov);
            let current = it.on;
            if hv > 0.01 || current {
                c.fill_rrect(row, 9.0, t.raised.mix(t.raised_hi, hv * 0.6).alpha(hv.max(current as u8 as f32 * 0.7)));
            }
            let danger = it.icon == Some(Icon::Trash);
            let ink = if danger {
                t.negative
            } else if it.dim {
                t.muted.mix(t.text2, hv)
            } else {
                t.text2.mix(t.text, hv.max(current as u8 as f32))
            };
            let cy = row.y + row.h * 0.5;
            let mut tx = row.x + 12.0;
            if let Some(icon) = it.icon {
                draw_icon(c, icon, row.x + 20.0, cy, 15.0, if danger { t.negative } else { t.muted.mix(t.accent, hv) });
                tx = row.x + 38.0;
            } else if let Some(col) = it.color
                && has_icons
            {
                // Parmi des lignes à icône : une pastille à la place de l'icône.
                c.fill_circle(row.x + 20.0, cy, 6.0, col);
                tx = row.x + 38.0;
            } else if let Some(col) = it.color {
                c.fill_rrect(Rect::new(row.x + 12.0, cy - 7.0, 4.0, 14.0), 2.0, col);
                tx = row.x + 26.0;
            }
            let mut right = row.right() - 12.0;
            if !it.detail.is_empty() {
                right = self.text_right(c, self.f.mono, 11.0, right, cy, &it.detail, t.muted) - 10.0;
            }
            self.text_fit(c, self.f.sans_med, 13.0, tx, cy, &it.name, right - tx, ink);
            if hov && !it.tip.is_empty() {
                hover_tip = Some(it.tip.clone());
            }
            if hov {
                self.set_cursor(ponzik_window::Cursor::Hand);
            }
            if hov && input.pressed {
                chosen = Some((i, row));
            }
        }
        c.set_clip(Some(prev));
        // Les menus gardent leurs infobulles : la description d'un instrument, un chemin.
        if let Some(tip) = hover_tip {
            let tw = self.fonts.measure(self.f.sans, 12.0, &tip).min(screen.w * 0.4);
            let tip_r = Rect::new((r.right() + 8.0).min(screen.right() - tw - 28.0), input.y - 14.0, tw + 20.0, 28.0);
            c.fill_rrect(tip_r, 8.0, t.raised_hi);
            c.stroke_rrect(tip_r, 8.0, 1.0, t.hairline.alpha(1.6));
            self.text_fit(c, self.f.sans, 12.0, tip_r.x + 10.0, tip_r.y + 14.0, &tip, tw, t.text2);
        }

        let clicked_outside = (input.pressed || input.right_pressed) && !r.contains(input.x, input.y);
        if let Some((i, row)) = chosen {
            self.anim.set(key, 0.0);
            let action = items[i].action.clone();
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

/// « Projet · il y a 3 min » : nom tiré du fichier de sauvegarde automatique
/// (`<nom>-AAAAMMJJ-HHMMSS-mmm`) et son âge.
fn recovery_detail(path: &std::path::Path) -> String {
    let stem = path.file_stem().map_or(String::new(), |s| s.to_string_lossy().into_owned());
    let name = stem.char_indices().rev().nth(19).map_or(stem.as_str(), |(i, _)| &stem[..i]).to_string();
    let age = std::fs::metadata(path).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).map(|d| d.as_secs());
    let age = match age {
        None => String::new(),
        Some(s) if s < 60 => tr("just now").into(),
        Some(s) if s < 3_600 => tr_fmt!("{} min ago", s / 60),
        Some(s) if s < 86_400 => tr_fmt!("{} h ago", s / 3_600),
        Some(s) => tr_fmt!("{} d ago", s / 86_400),
    };
    if age.is_empty() { name } else { format!("{name} · {age}") }
}

/// Icône d'un modèle de projet, dans l'ordre de [`ponzik_project::templates`].
fn template_icon(i: usize) -> Icon {
    match i {
        0 => Icon::Plus,
        1 => Icon::Rack,
        2 => Icon::Session,
        3 => Icon::Sample,
        4 => Icon::Metronome,
        _ => Icon::Effect,
    }
}

/// Noms des toniques ; « # » et « b » : les polices embarquées n'ont pas ♯ ni ♭.
pub fn key_name(key: u8) -> &'static str {
    match key % 12 {
        0 => tr_ctx("key", "C"),
        1 => tr_ctx("key", "C#"),
        2 => tr_ctx("key", "D"),
        3 => tr_ctx("key", "Eb"),
        4 => tr_ctx("key", "E"),
        5 => tr_ctx("key", "F"),
        6 => tr_ctx("key", "F#"),
        7 => tr_ctx("key", "G"),
        8 => tr_ctx("key", "Ab"),
        9 => tr_ctx("key", "A"),
        10 => tr_ctx("key", "Bb"),
        _ => tr_ctx("key", "B"),
    }
}

/// « A minor », « D dorian » ; « Chromatic » sans gamme.
fn scale_label(key: u8, scale: ScaleKind) -> String {
    let mode = match scale {
        ScaleKind::Chromatic => return tr("Chromatic").into(),
        ScaleKind::Major => tr("major"),
        ScaleKind::Minor => tr("minor"),
        ScaleKind::Dorian => tr("dorian"),
        ScaleKind::Phrygian => tr("phrygian"),
        ScaleKind::Lydian => tr("lydian"),
        ScaleKind::Mixolydian => tr("mixolydian"),
        ScaleKind::HarmonicMinor => tr("harm. minor"),
        ScaleKind::MelodicMinor => tr("mel. minor"),
        ScaleKind::PentatonicMajor => tr("maj. penta"),
        ScaleKind::PentatonicMinor => tr("min. penta"),
        ScaleKind::Blues => tr("blues"),
        ScaleKind::Locrian => tr("locrian"),
        ScaleKind::HarmonicMajor => tr("harm. major"),
        ScaleKind::BluesMajor => tr("major blues"),
        ScaleKind::PhrygianDominant => tr("phryg. dom."),
        ScaleKind::DoubleHarmonic => tr("double harm."),
        ScaleKind::HungarianMinor => tr("hungarian"),
        ScaleKind::Hirajoshi => tr("hirajoshi"),
        ScaleKind::InSen => tr("in sen"),
        ScaleKind::WholeTone => tr("whole tone"),
        ScaleKind::Diminished => tr("diminished"),
        ScaleKind::BebopDominant => tr("bebop"),
    };
    tr_fmt!("{} {}", key_name(key), mode)
}

/// Tempo des appuis (instants en secondes) : 60 / intervalle moyen. `None` avant
/// le deuxième appui ou si l'intervalle est nul.
fn tap_bpm(taps: &[f32]) -> Option<f32> {
    let (first, last) = (*taps.first()?, *taps.last()?);
    let span = (last - first) / (taps.len() - 1).max(1) as f32;
    (taps.len() >= 2 && span > 1e-3).then(|| 60.0 / span)
}

/// Sonie-mètre du master, sur le flux mono (moitié de la fréquence d'échantillonnage).
/// Le flux est la moyenne (G + D) / 2 : pour un signal centré, son énergie est la
/// moitié de la somme stéréo de BS.1770. Le poids 2 du canal rend la mesure stéréo.
fn master_loudness(sample_rate: u32) -> LoudnessMeter {
    let mut m = LoudnessMeter::new((sample_rate / SCOPE_DECIMATION) as f32, 1);
    m.set_channel_weight(0, 2.0);
    m
}

/// Oublie les plus vieilles étapes au-delà de `steps` étapes ou de `bytes`
/// octets (estimés), en gardant toujours la dernière.
fn trim_history(past: &mut Vec<(String, Project)>, steps: usize, bytes: usize) {
    let mut total: usize = past.iter().map(|(_, p)| p.approx_bytes()).sum();
    let mut old = 0;
    while past.len() - old > 1 && (past.len() - old > steps || total > bytes) {
        total -= past[old].1.approx_bytes();
        old += 1;
    }
    past.drain(..old);
}

/// Analyseur du master : 4096 points, 75 % de recouvrement, pente de 4,5 dB/octave
/// comme les analyseurs d'EQ (un mix équilibré paraît à plat).
fn master_spectrum(sample_rate: u32) -> SpectrumAnalyzer {
    let mut s = SpectrumAnalyzer::new((sample_rate / SCOPE_DECIMATION) as f32, SPECTRUM_SIZE, 0.75);
    s.set_tilt(4.5);
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_detail_strips_the_timestamp() {
        let detail = recovery_detail(std::path::Path::new("/nulle/part/Mon-morceau-20261116-143200-123.ponzik"));
        assert_eq!(detail, "Mon-morceau");
    }

    #[test]
    fn undo_history_is_bounded_in_steps_and_memory() {
        let mut big = Project::empty();
        let note = ponzik_engine::Note { start: 0, len: 24, key: 60, vel: 0.8 };
        big.patterns[0].notes = vec![vec![note; 10_000]; 4];
        let size = big.approx_bytes();
        assert!(size > 40_000 * std::mem::size_of::<ponzik_engine::Note>(), "{size}");
        let mut past: Vec<(String, Project)> = Vec::new();
        for i in 0..50 {
            past.push((i.to_string(), big.clone()));
            trim_history(&mut past, 200, size * 10 + size / 2);
        }
        // Ten copies fit in the budget; the newest ones are kept.
        assert_eq!(past.len(), 10);
        assert_eq!(past.last().unwrap().0, "49");
        let small = Project::empty();
        for i in 0..300 {
            past.push((i.to_string(), small.clone()));
            trim_history(&mut past, 200, usize::MAX);
        }
        assert_eq!(past.len(), 200);
        // Even a single step above the budget is kept.
        past.truncate(1);
        trim_history(&mut past, 200, 0);
        assert_eq!(past.len(), 1);
    }

    #[test]
    fn tap_tempo_averages_the_intervals() {
        assert_eq!(tap_bpm(&[]), None);
        assert_eq!(tap_bpm(&[1.0]), None);
        assert!((tap_bpm(&[0.0, 0.5]).unwrap() - 120.0).abs() < 1e-3);
        // Intervalles 0,5 · 0,4 · 0,6 : moyenne 0,5 s.
        assert!((tap_bpm(&[0.0, 0.5, 0.9, 1.5]).unwrap() - 120.0).abs() < 1e-3);
    }

    #[test]
    fn scale_labels_are_short() {
        ponzik_i18n::with_lang(ponzik_i18n::Lang::EN, || {
            assert_eq!(scale_label(9, ScaleKind::Minor), "A minor");
            assert_eq!(scale_label(1, ScaleKind::Major), "C# major");
            assert_eq!(scale_label(4, ScaleKind::Chromatic), "Chromatic");
        });
        ponzik_i18n::with_lang(ponzik_i18n::Lang::from_code("fr").unwrap(), || {
            assert_eq!(scale_label(9, ScaleKind::Minor), "La mineur");
            assert_eq!(scale_label(1, ScaleKind::Major), "Do# majeur");
            assert_eq!(scale_label(4, ScaleKind::Chromatic), "Chromatique");
        });
    }
}
