//! Modèle de projet Ponzik : la source de vérité de l'interface.
//!
//! Le projet décrit tout (canaux, patterns, playlist, mixer) ; il sait construire
//! un moteur complet et produire l'instantané de séquence à envoyer à chaque
//! modification.

use std::sync::Arc;

use ponzik_i18n::{tr, tr_fmt, tr_noop};

use ponzik_engine::mixer::{DEFAULT_TRACKS, INSERT_SLOTS, MASTER, SEND_SLOTS, routing_order};
use ponzik_engine::registry;
use ponzik_engine::{
    AudioClip, AutomationLane, Channel, Clip, ClipWarp, Effect, Engine, EngineHandle, Frozen, Instrument, NOTE_FX_SLOTS,
    Note, NoteFx, PPQ, Pattern, PlayMode, Sample, Sequence, TICKS_PER_BAR, TICKS_PER_STEP,
};
pub use ponzik_dsp::warp::WarpAlgo;
pub use ponzik_engine::{WarpMap, WarpMarker};
pub use ponzik_engine::{TempoMap, TempoPoint, TimeSig};
pub use ponzik_engine::{AutoPoint, AutoScope, AutoTarget, SessionSlot};

/// Dimensions de la grille de la vue Session.
/// Lanes of the playlist (as many as FL Studio's playlist tracks).
pub const PLAYLIST_LANES: usize = 500;
pub const SESSION_COLUMNS: usize = 8;
pub const SESSION_SCENES: usize = 8;

pub mod ab;
pub mod als;
mod arrange;
pub mod base64;
pub mod bounce;
pub mod auto_rec;
pub mod autosave;
pub mod capture;
pub mod comp;
mod breakcore;
mod zalgo;
mod demo;
pub mod history;
pub mod io;
pub mod flp;
pub mod generate;
pub mod gzip;
pub mod groove;
pub mod harmony;
pub mod json;
pub mod library;
pub mod media;
pub mod midi_file;
pub mod midi_map;
pub mod plugins;
pub mod presets;
pub mod rack;
pub mod render;
pub mod scale;
pub mod session;
pub mod templates;
mod tracks;
pub mod transform;
pub mod update;
pub mod voice;
pub mod warp;

pub use arrange::{MARKER_COLORS, Placement, TimeSlice};
pub use bounce::{BounceSource, FreezeTarget, SourceAction, frozen_content_changed};
pub use breakcore::demo_breakcore;
pub use comp::{CompSegment, Take, TakeGroup};
pub use zalgo::demo_zalgo;
pub use demo::demo_project;
pub use history::History;
pub use flp::{ImportReport, ReportGroup, ReportKind, export_flp, import_flp, resolve_sample};
pub use io::{load, load_from_file, save, save_to_file};
pub use midi_file::{MidiFile, MidiTrack, import_midi, read_smf, write_smf};
pub use render::{
    BitDepth, ExportOptions, FileFormat, Levels, Normalize, RenderMode, export_audio, export_wav, normalize, region_file_name,
    fast_render, render_regions, render_song, render_stems, set_fast_render, write_audio, write_wav, write_wav_channels,
};
pub use rack::{ModTarget, ModulatorDef, RackDef, RackDevice};
pub use scale::{ChordKind, ScaleKind, Voicing, chord, revoice};
pub use session::{Follow, FollowAction, LaunchMode, LiveClip, Quant, SceneDef, SlotDef};
pub use templates::templates;

/// Un instrument et l'état de ses paramètres.
#[derive(Clone, Debug, PartialEq)]
pub struct InstrumentDef {
    /// Identifiant du registre natif, `clap:<chemin>#<id>` pour un plugin CLAP
    /// (voir [`ponzik_clap::kind_for`]) ou `vst3:<chemin>#<classe>` pour un
    /// plugin VST 3 (voir [`ponzik_vst3::kind_for`]).
    pub kind: String,
    /// Valeurs qui diffèrent des défauts : (identifiant, valeur).
    pub params: Vec<(u32, f32)>,
    /// Fichier audio chargé (sampler).
    pub sample: Option<String>,
    /// Pad instruments (Drum Pads): the files of each pad, one per velocity
    /// layer, softest first. Empty for every other instrument.
    pub pads: Vec<Vec<String>>,
    /// The plugin's opaque state ([`PluginState`]), captured when the project
    /// is saved; `None` for native instruments.
    pub state: Option<PluginState>,
    /// The other A/B state, once compared (see [`ab`]).
    pub ab: Option<Box<ab::DeviceAb>>,
}

/// Opaque state of a CLAP or VST 3 plugin (`clap.state`, VST 3 component and
/// controller states): the whole patch, parameters included. Base64 in the
/// project file. Shared, so that undo snapshots and copies of the project do
/// not duplicate it.
pub type PluginState = Arc<[u8]>;

impl InstrumentDef {
    pub fn new(kind: &str) -> Self {
        Self { kind: kind.into(), params: Vec::new(), sample: None, pads: Vec::new(), state: None, ab: None }
    }

    /// Every audio file the instrument needs (sample and pad layers).
    pub fn sample_paths(&self) -> impl Iterator<Item = &String> {
        self.sample.iter().chain(self.pads.iter().flatten())
    }

    pub fn with(mut self, id: u32, value: f32) -> Self {
        self.set(id, value);
        self
    }

    pub fn set(&mut self, id: u32, value: f32) {
        match self.params.iter_mut().find(|(i, _)| *i == id) {
            Some(p) => p.1 = value,
            None => self.params.push((id, value)),
        }
    }

    pub fn get(&self, id: u32) -> Option<f32> {
        self.params.iter().find(|(i, _)| *i == id).map(|p| p.1)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct EffectDef {
    /// Comme [`InstrumentDef::kind`] : natif, `clap:…` ou `vst3:…`.
    pub kind: String,
    pub params: Vec<(u32, f32)>,
    pub bypass: bool,
    /// Piste clé de l'entrée latérale (compresseur, porte, ducking…).
    pub sidechain: Option<usize>,
    /// Chains and macro mappings, for a rack (kind `"rack"`).
    pub rack: Option<Box<RackDef>>,
    /// Like [`InstrumentDef::state`].
    pub state: Option<PluginState>,
    /// Like [`InstrumentDef::ab`].
    pub ab: Option<Box<ab::DeviceAb>>,
}

impl EffectDef {
    pub fn new(kind: &str) -> Self {
        Self {
            kind: kind.into(),
            params: Vec::new(),
            bypass: false,
            sidechain: None,
            rack: (kind == rack::RACK).then(Box::default),
            state: None,
            ab: None,
        }
    }

    pub fn set(&mut self, id: u32, value: f32) {
        match self.params.iter_mut().find(|(i, _)| *i == id) {
            Some(p) => p.1 = value,
            None => self.params.push((id, value)),
        }
    }

    pub fn get(&self, id: u32) -> Option<f32> {
        self.params.iter().find(|(i, _)| *i == id).map(|p| p.1)
    }
}

/// Un effet de notes (arpégiateur, accord…) et ses paramètres.
#[derive(Clone, Debug, PartialEq)]
pub struct NoteFxDef {
    /// Identifiant du registre natif ([`registry::note_fx`]).
    pub kind: String,
    /// Valeurs qui diffèrent des défauts : (identifiant, valeur).
    pub params: Vec<(u32, f32)>,
}

impl NoteFxDef {
    pub fn new(kind: &str) -> Self {
        Self { kind: kind.into(), params: Vec::new() }
    }

    pub fn with(mut self, id: u32, value: f32) -> Self {
        self.set(id, value);
        self
    }

    pub fn set(&mut self, id: u32, value: f32) {
        match self.params.iter_mut().find(|(i, _)| *i == id) {
            Some(p) => p.1 = value,
            None => self.params.push((id, value)),
        }
    }

    pub fn get(&self, id: u32) -> Option<f32> {
        self.params.iter().find(|(i, _)| *i == id).map(|p| p.1)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChannelDef {
    pub name: String,
    pub instrument: InstrumentDef,
    /// Effets de notes avant l'instrument, dans l'ordre de la chaîne ; au plus
    /// [`NOTE_FX_SLOTS`] emplacements (les suivants sont ignorés), vides compris.
    pub note_fx: Vec<Option<NoteFxDef>>,
    pub gain: f32,
    pub pan: f32,
    pub muted: bool,
    /// Piste de mixer.
    pub track: usize,
    /// Note posée par un clic dans le step sequencer.
    pub root_note: u8,
    pub color: u32,
    /// Modulators of the instrument's parameters.
    pub modulators: Vec<ModulatorDef>,
    /// Frozen: the audio file of its render, played instead of its
    /// instrument and note effects (kept here, unloaded from the engine).
    pub frozen: Option<String>,
    /// Portamento in seconds (0: off): each note glides from the previous one.
    pub porta: f32,
}

/// Longest channel portamento, in seconds.
pub const MAX_PORTA: f32 = 2.0;

#[derive(Clone, Debug, PartialEq)]
pub struct PatternDef {
    pub name: String,
    /// Longueur en ticks.
    pub len: u32,
    /// Notes par canal, triées par début.
    pub notes: Vec<Vec<Note>>,
    pub color: u32,
    /// Groove the pattern plays with, its notes untouched.
    pub groove: Option<groove::PatternGroove>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClipDef {
    pub pattern: usize,
    /// Ligne de la playlist (affichage).
    pub lane: usize,
    pub start: u32,
    pub len: u32,
    pub offset: u32,
    /// Kept in the playlist, not played (a bounced source, for instance).
    pub muted: bool,
}

/// Courbe d'automation : points triés par tick, un seul point par tick.
#[derive(Clone, Debug, PartialEq)]
pub struct AutomationDef {
    pub target: AutoTarget,
    pub scope: AutoScope,
    pub points: Vec<AutoPoint>,
}

impl AutomationDef {
    pub fn value_at(&self, tick: f64) -> Option<f32> {
        ponzik_engine::sequence::automation_value(&self.points, tick)
    }
}

/// Fichier audio posé dans la playlist.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioClipDef {
    pub path: String,
    /// Ligne de la playlist (affichage).
    pub lane: usize,
    /// Piste de mixer.
    pub track: usize,
    pub start: u32,
    pub len: u32,
    /// Décalage dans le fichier, en secondes.
    pub offset: f64,
    pub gain: f32,
    /// Fondus en secondes.
    pub fade_in: f32,
    pub fade_out: f32,
    /// Fade shapes in [−1, 1] (0: linear).
    pub fade_in_curve: f32,
    pub fade_out_curve: f32,
    /// Groupe de prises (0 : aucun) : les passages d'un même enregistrement en
    /// boucle le partagent, pour choisir la bonne (et compiler plus tard).
    pub take: u32,
    /// Clip muet : gardé dans la playlist (prise de côté), pas joué.
    pub muted: bool,
    /// Played backwards (offset and warp markers count in the reversed file).
    pub reverse: bool,
    /// The file starts over past its end.
    pub looped: bool,
    /// 0: the default audio clip color.
    pub color: u32,
    /// Warped: the beats of the file follow the song tempo.
    pub warp: bool,
    /// Original tempo of the file (0: unknown, 120 is used).
    pub bpm: f32,
    pub warp_mode: WarpAlgo,
    /// Transposition in semitones and cents (Beats and Tones; Re-Pitch follows the tempo).
    pub transpose: f32,
    pub fine: f32,
    /// Points of the file pinned to beats (beat 0: the file start).
    pub warp_markers: Vec<WarpMarker>,
}

impl Default for AudioClipDef {
    fn default() -> Self {
        Self {
            path: String::new(),
            lane: 0,
            track: 1,
            start: 0,
            len: TICKS_PER_BAR,
            offset: 0.0,
            gain: 1.0,
            fade_in: 0.002,
            fade_out: 0.002,
            fade_in_curve: 0.0,
            fade_out_curve: 0.0,
            take: 0,
            muted: false,
            reverse: false,
            looped: false,
            color: 0,
            warp: false,
            bpm: 0.0,
            warp_mode: WarpAlgo::Beats,
            transpose: 0.0,
            fine: 0.0,
            warp_markers: Vec::new(),
        }
    }
}

impl AudioClipDef {
    /// Original tempo used for warping.
    pub fn warp_bpm(&self) -> f64 {
        if self.bpm > 0.0 && self.bpm.is_finite() { self.bpm as f64 } else { 120.0 }
    }

    pub fn warp_map(&self) -> WarpMap {
        WarpMap::new(self.warp_bpm(), self.warp_markers.clone())
    }

    /// Frequency ratio of the transposition.
    pub fn pitch_ratio(&self) -> f64 {
        ((self.transpose as f64 + self.fine as f64 / 100.0) / 12.0).exp2()
    }

    /// Point of the file (seconds) heard at `tick` (absolute), ignoring loops.
    pub fn offset_at(&self, tick: f64, map: &TempoMap) -> f64 {
        let from = self.start as f64;
        if self.warp {
            let w = self.warp_map();
            w.secs_at(w.beat_at(self.offset) + (tick - from) / PPQ as f64)
        } else {
            self.offset + map.secs_at(tick) - map.secs_at(from)
        }
    }

    /// Length in ticks that plays the file from the offset to its end
    /// (`duration` in seconds), at least a sixteenth.
    pub fn natural_len(&self, duration: f64, map: &TempoMap) -> u32 {
        let ticks = if self.warp {
            let w = self.warp_map();
            (w.beat_at(duration) - w.beat_at(self.offset)) * PPQ as f64
        } else {
            let from = map.secs_at(self.start as f64);
            map.tick_at_secs(from + (duration - self.offset).max(0.0)) - self.start as f64
        };
        ticks.round().max(TICKS_PER_STEP as f64) as u32
    }

    /// Ticks (absolute) inside the clip where a looped file starts over, at
    /// most `max` of them; empty when the clip does not loop.
    pub fn loop_starts(&self, duration: f64, map: &TempoMap, max: usize) -> Vec<f64> {
        let mut out = Vec::new();
        if !self.looped || duration <= 0.0 {
            return out;
        }
        let (start, end) = (self.start as f64, self.start as f64 + self.len as f64);
        if self.warp {
            let w = self.warp_map();
            let (b0, b1, at) = (w.beat_at(0.0), w.beat_at(duration), w.beat_at(self.offset.rem_euclid(duration)));
            let period = (b1 - b0) * PPQ as f64;
            if period < 1.0 {
                return out;
            }
            let mut tick = start + (b1 - at) * PPQ as f64;
            while tick < end - 0.5 && out.len() < max {
                out.push(tick);
                tick += period;
            }
        } else {
            let from = map.secs_at(start);
            let mut secs = duration - self.offset.rem_euclid(duration);
            loop {
                let tick = map.tick_at_secs(from + secs);
                if tick >= end - 0.5 || out.len() >= max {
                    break;
                }
                out.push(tick);
                secs += duration;
            }
        }
        out
    }

    /// Length in ticks of one pass of the whole file from the clip start (a
    /// repetition of a looped clip).
    pub fn loop_period(&self, duration: f64, map: &TempoMap) -> u32 {
        let ticks = if self.warp {
            let w = self.warp_map();
            (w.beat_at(duration) - w.beat_at(0.0)) * PPQ as f64
        } else {
            let from = map.secs_at(self.start as f64);
            map.tick_at_secs(from + duration.max(0.0)) - self.start as f64
        };
        ticks.round().max(TICKS_PER_STEP as f64) as u32
    }
}

/// Rôle d'une piste de mixer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TrackKind {
    /// Piste ordinaire : reçoit des canaux, des clips audio ou d'autres pistes.
    #[default]
    Normal,
    /// Bus : d'autres pistes y sortent ; ses inserts traitent leur somme.
    Bus,
    /// VCA : aucun son ne le traverse, son fader pilote les pistes qui le suivent.
    Vca,
}

impl TrackKind {
    pub fn id(self) -> &'static str {
        match self {
            TrackKind::Normal => "normal",
            TrackKind::Bus => "bus",
            TrackKind::Vca => "vca",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        [TrackKind::Normal, TrackKind::Bus, TrackKind::Vca].into_iter().find(|k| k.id() == id)
    }
}

/// Couleurs données tour à tour aux nouveaux bus et VCA.
pub const BUS_COLORS: [u32; 6] = [0xf0b44a, 0xa98cf0, 0x2fb6e0, 0xe878b4, 0x3ccfc4, 0xf08a5d];

/// Effet d'un bus de retour : type, nom du bus (anglais, traduit à l'affichage
/// et à la création), réglages (100 % mouillé).
pub type ReturnEffect = (&'static str, &'static str, &'static [(u32, f32)]);

/// Effets proposés pour un bus de retour.
pub const RETURN_EFFECTS: [ReturnEffect; 2] = [
    // Réverbe : salle moyenne, pré-délai qui détache la voix, aigus adoucis.
    ("reverb", tr_noop("Reverb"), &[(0, 0.03), (1, 0.7), (2, 2.2), (3, 0.55), (5, 1.0)]),
    // Délai : croche pointée en ping-pong, répétitions filtrées.
    ("delay", tr_noop("Delay"), &[(0, 9.0), (2, 0.35), (3, 1.0), (4, 200.0), (5, 6_000.0), (6, 1.0)]),
];

#[derive(Clone, Debug)]
pub struct TrackDef {
    /// Identity of the track while the project is open (not saved): it
    /// follows the track when tracks are added, deleted or moved, so that the
    /// engine can move the track's state with it. Unique in the process.
    pub id: u32,
    pub name: String,
    pub gain: f32,
    pub pan: f32,
    pub muted: bool,
    pub output: usize,
    pub sends: [(usize, f32); SEND_SLOTS],
    pub inserts: [Option<EffectDef>; INSERT_SLOTS],
    pub color: u32,
    pub kind: TrackKind,
    /// Bus replié : les pistes qui y sortent sont cachées sous lui dans le mixer.
    pub folded: bool,
    /// VCA qui pilote le volume de la piste.
    pub vca: Option<usize>,
    /// Modulators of the track's inserts.
    pub modulators: Vec<ModulatorDef>,
    /// Frozen: the audio file of its render (everything that reaches it,
    /// through its inserts), played instead of its input and inserts.
    pub frozen: Option<String>,
}

/// Nom par défaut d'une piste de mixer (« Track 3 », « Piste 3 » en français).
pub fn default_track_name(index: usize) -> String {
    tr_fmt!("Track {}", index)
}

/// Vrai si `name` est un nom de piste par défaut, dans la langue courante ou
/// dans une autre (projets enregistrés en anglais ou en français).
pub fn is_default_track_name(name: &str) -> bool {
    let current = tr_fmt!("Track {}", "");
    [current.as_str(), "Track ", "Piste "].iter().any(|p| name.starts_with(p))
}

/// Content equality: the identity of a track does not count.
impl PartialEq for TrackDef {
    fn eq(&self, o: &Self) -> bool {
        let Self { id: _, name, gain, pan, muted, output, sends, inserts, color, kind, folded, vca, modulators, frozen } = self;
        *name == o.name
            && *gain == o.gain
            && *pan == o.pan
            && *muted == o.muted
            && *output == o.output
            && *sends == o.sends
            && *inserts == o.inserts
            && *color == o.color
            && *kind == o.kind
            && *folded == o.folded
            && *vca == o.vca
            && *modulators == o.modulators
            && *frozen == o.frozen
    }
}

/// Next track identity.
static NEXT_TRACK_ID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

impl TrackDef {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: NEXT_TRACK_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            name: name.into(),
            gain: 1.0,
            pan: 0.0,
            muted: false,
            output: MASTER,
            sends: [(MASTER, 0.0); SEND_SLOTS],
            inserts: std::array::from_fn(|_| None),
            color: 0,
            kind: TrackKind::Normal,
            folded: false,
            vca: None,
            modulators: Vec::new(),
            frozen: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Project {
    pub name: String,
    pub bpm: f32,
    pub swing: f32,
    /// Tonalité (0 = do … 11 = si) et mode de la gamme du projet.
    pub key: u8,
    pub scale: ScaleKind,
    pub channels: Vec<ChannelDef>,
    pub patterns: Vec<PatternDef>,
    pub clips: Vec<ClipDef>,
    /// Mixer tracks, the master first ([`DEFAULT_TRACKS`] in a new project).
    pub tracks: Vec<TrackDef>,
    pub mode: PlayMode,
    pub current_pattern: usize,
    /// Boucle de lecture tirée dans une règle (non enregistrée dans le fichier).
    pub loop_range: Option<(u32, u32)>,
    /// Grille de la vue Session : `session_grid[colonne][scène]` = pattern.
    pub session_grid: Vec<Vec<Option<usize>>>,
    /// Launch settings and audio clips of the slots, `[column][scene]`.
    pub session_slots: Vec<Vec<SlotDef>>,
    /// Settings of the scenes (rows).
    pub session_scenes: Vec<SceneDef>,
    /// Quantization of the launches (slots can have their own).
    pub session_quant: Quant,
    /// Clips de session en cours de lecture (non enregistrés).
    pub session_live: Vec<LiveClip>,
    pub automation: Vec<AutomationDef>,
    pub audio_clips: Vec<AudioClipDef>,
    /// Loop recordings: takes and the comp that plays (see [`comp`]).
    pub take_groups: Vec<TakeGroup>,
    /// Tempo changes after tick 0, sorted (the tempo at tick 0 is `bpm`).
    pub tempo_changes: Vec<TempoPoint>,
    /// Time signature changes, sorted (4/4 from tick 0 unless one is there).
    pub signatures: Vec<TimeSig>,
    /// Arrangement markers, sorted by tick: each one starts a section that
    /// runs to the next marker (the last one to the end of the song).
    pub markers: Vec<Marker>,
    /// MIDI controller links saved with this project (see [`midi_map`]).
    pub midi_map: Vec<midi_map::MidiMapping>,
    /// Grooves extracted into the project's pool (after the built-in ones).
    pub grooves: Vec<groove::Groove>,
}

/// A named section start on the arrangement lane.
#[derive(Clone, Debug, PartialEq)]
pub struct Marker {
    pub tick: u32,
    pub name: String,
    /// 0: the default color for its place.
    pub color: u32,
}

/// What a sampler's tempo sync depends on (its length in beats, the
/// tempo), `None` when it does not follow the tempo.
pub fn sync_key(def: &InstrumentDef, bpm: f32) -> Option<(u32, u32)> {
    use ponzik_engine::instruments::{SAMPLER_BEATS, SAMPLER_SYNC};
    let beats = def.get(SAMPLER_BEATS).unwrap_or(0.0);
    (def.kind == "sampler" && def.get(SAMPLER_SYNC).unwrap_or(0.0) > 0.5 && beats > 0.0 && bpm > 0.0).then(|| (beats.to_bits(), bpm.to_bits()))
}

/// The sample a sampler plays: in tempo sync, its `beats` stretched (pitch
/// kept, by the warp engine's Beats mode) to last as long at `bpm`.
pub fn synced_sample(def: &InstrumentDef, sample: Arc<Sample>, bpm: f32) -> Arc<Sample> {
    use ponzik_engine::instruments::SAMPLER_BEATS;
    let duration = sample.duration() as f64;
    if sync_key(def, bpm).is_none() || duration <= 0.0 {
        return sample;
    }
    let beats = def.get(SAMPLER_BEATS).unwrap_or(0.0) as f64;
    let source_bpm = beats * 60.0 / duration;
    let clip = AudioClipDef {
        warp: true,
        bpm: source_bpm as f32,
        len: (beats * PPQ as f64).round() as u32,
        warp_mode: WarpAlgo::Beats,
        ..Default::default()
    };
    let map = TempoMap::constant(bpm);
    match warp::WarpJob::for_clip(&clip, &sample, &map, sample.sample_rate) {
        Some(job) => warp::render_cached(&job),
        None => sample,
    }
}

/// Fonction de chargement de fichiers audio (fournie par l'application).
pub type SampleLoader<'a> = &'a dyn Fn(&str) -> Option<Arc<Sample>>;

/// Render of a warped clip, if available.
pub type WarpLookup<'a> = &'a dyn Fn(&warp::WarpJob) -> Option<Arc<Sample>>;

/// Renders warped clips now (through the shared cache): exports and offline renders.
pub fn render_warps(job: &warp::WarpJob) -> Option<Arc<Sample>> {
    Some(warp::render_cached(job))
}

impl Default for Project {
    fn default() -> Self {
        Self::empty()
    }
}

impl Project {
    pub fn empty() -> Self {
        let mut tracks: Vec<TrackDef> = (0..DEFAULT_TRACKS).map(|i| TrackDef::new(default_track_name(i))).collect();
        tracks[MASTER].name = tr("Master").into();
        Self {
            name: tr("Untitled").into(),
            bpm: 120.0,
            swing: 0.0,
            key: 0,
            scale: ScaleKind::Chromatic,
            channels: Vec::new(),
            patterns: vec![PatternDef { name: tr_fmt!("Pattern {}", 1), len: TICKS_PER_BAR, notes: Vec::new(), color: 0, groove: None }],
            clips: Vec::new(),
            tracks,
            mode: PlayMode::Pattern(0),
            current_pattern: 0,
            loop_range: None,
            session_grid: vec![vec![None; SESSION_SCENES]; SESSION_COLUMNS],
            session_slots: vec![vec![SlotDef::DEFAULT; SESSION_SCENES]; SESSION_COLUMNS],
            session_scenes: vec![SceneDef::default(); SESSION_SCENES],
            session_quant: Quant::Bar,
            session_live: Vec::new(),
            automation: Vec::new(),
            audio_clips: Vec::new(),
            take_groups: Vec::new(),
            tempo_changes: Vec::new(),
            signatures: Vec::new(),
            markers: Vec::new(),
            midi_map: Vec::new(),
            grooves: Vec::new(),
        }
    }

    /// Mémoire occupée par le projet, à peu près (tailles des listes et des
    /// textes, sans les parcourir élément par élément) : de quoi borner
    /// l'historique d'annulation, qui en garde des copies.
    pub fn approx_bytes(&self) -> usize {
        use std::mem::{size_of, size_of_val};
        fn vec<T>(v: &[T]) -> usize {
            size_of_val(v)
        }
        let params = |p: &[(u32, f32)]| vec(p);
        let channels: usize = self
            .channels
            .iter()
            .map(|c| {
                let i = &c.instrument;
                let pads: usize = i.pads.iter().map(|p| vec(p) + p.iter().map(String::len).sum::<usize>()).sum();
                let fx: usize = c.note_fx.iter().flatten().map(|f| f.kind.len() + params(&f.params)).sum();
                c.name.len() + i.kind.len() + params(&i.params) + i.sample.as_ref().map_or(0, String::len) + pads + fx + vec(&c.note_fx)
            })
            .sum();
        let patterns: usize =
            self.patterns.iter().map(|p| p.name.len() + vec(&p.notes) + p.notes.iter().map(|n| vec(n)).sum::<usize>()).sum();
        let tracks: usize = self
            .tracks
            .iter()
            .map(|t| t.name.len() + t.inserts.iter().flatten().map(|e| e.kind.len() + params(&e.params)).sum::<usize>())
            .sum();
        let automation: usize = self.automation.iter().map(|a| vec(&a.points)).sum();
        let audio: usize = self.audio_clips.iter().map(|c| c.path.len()).sum();
        let takes: usize = self
            .take_groups
            .iter()
            .map(|g| vec(&g.takes) + vec(&g.comp) + g.takes.iter().map(|t| t.name.len() + t.clip.path.len()).sum::<usize>())
            .sum();
        let grid: usize = self.session_grid.iter().map(|c| vec(c)).sum();
        size_of::<Self>()
            + self.name.len()
            + vec(&self.channels)
            + channels
            + vec(&self.patterns)
            + patterns
            + vec(&self.clips)
            + vec(&self.tracks)
            + tracks
            + vec(&self.automation)
            + automation
            + vec(&self.audio_clips)
            + audio
            + vec(&self.take_groups)
            + takes
            + vec(&self.session_grid)
            + grid
            + vec(&self.session_live)
            + self.session_slots.iter().map(|c| vec(c)).sum::<usize>()
    }

    /// Fin du morceau : fin du dernier clip (pattern ou audio), arrondie à la
    /// mesure (of the time signature map), one bar at least.
    pub fn song_len(&self) -> u32 {
        let patterns = self.clips.iter().map(|c| c.start.saturating_add(c.len));
        let audio = self.audio_clips.iter().map(|c| c.start.saturating_add(c.len));
        let takes = self.take_groups.iter().map(|g| g.span().1);
        let end = patterns.chain(audio).chain(takes).max().unwrap_or(0);
        let map = self.tempo_map();
        map.ceil_bar(end as f64).max(map.bar_start(1))
    }

    /// Instantané de séquence pour le moteur, sans les clips audio (aucun
    /// échantillon chargé) ; voir [`Project::sequence_with`].
    pub fn sequence(&self) -> Sequence {
        self.sequence_with(&|_| None)
    }

    /// Tempo map of the song: the project tempo, or the song tempo automation
    /// lane when there is one (its points become tempo points, straight
    /// segments become ramps and curved ones a chain of short ramps).
    pub fn tempo_map(&self) -> TempoMap {
        TempoMap::new(self.tempo_points(), self.signatures.clone())
    }

    /// The tempo map drawn on the tempo lane: `bpm` and the tempo changes,
    /// without the tempo automation lane.
    pub fn tempo_lane_map(&self) -> TempoMap {
        let mut points = vec![TempoPoint { tick: 0, bpm: self.bpm, ramp: false }];
        points.extend(self.tempo_changes.iter().filter(|p| p.tick > 0));
        TempoMap::new(points, self.signatures.clone())
    }

    /// True when a song tempo automation lane overrides the tempo lane.
    pub fn tempo_automated(&self) -> bool {
        self.automation_lane(AutoTarget::Tempo, AutoScope::Song).is_some_and(|l| !l.points.is_empty())
    }

    fn tempo_points(&self) -> Vec<TempoPoint> {
        let Some(lane) = self.automation_lane(AutoTarget::Tempo, AutoScope::Song).filter(|l| !l.points.is_empty()) else {
            return self.tempo_lane_map().tempos().to_vec();
        };
        const PIECES: u32 = 16;
        let mut out = vec![TempoPoint { tick: 0, bpm: lane.points[0].value, ramp: false }];
        for (i, p) in lane.points.iter().enumerate() {
            match i.checked_sub(1).map(|j| lane.points[j]) {
                Some(a) if a.curve.abs() > 1e-6 && p.tick > a.tick + 1 => {
                    let span = p.tick - a.tick;
                    for k in 1..PIECES {
                        let tick = a.tick + span * k / PIECES;
                        if let Some(bpm) = lane.value_at(tick as f64) {
                            out.push(TempoPoint { tick, bpm, ramp: true });
                        }
                    }
                    out.push(TempoPoint { tick: p.tick, bpm: p.value, ramp: true });
                }
                Some(_) => out.push(TempoPoint { tick: p.tick, bpm: p.value, ramp: true }),
                None => out.push(TempoPoint { tick: p.tick, bpm: p.value, ramp: false }),
            }
        }
        out
    }

    /// Instantané de séquence complet : les clips audio dont le fichier ne se
    /// charge pas sont ignorés. Warped clips get no render: they play
    /// resampled (see [`Project::sequence_for`]).
    pub fn sequence_with(&self, loader: SampleLoader) -> Sequence {
        self.sequence_for(0.0, loader, &|_| None)
    }

    /// Renders needed by the warped clips at this engine rate.
    pub fn warp_jobs(&self, sample_rate: f32, loader: SampleLoader) -> Vec<warp::WarpJob> {
        let map = self.tempo_map();
        self.audio_clips
            .iter()
            .filter(|c| !c.muted)
            .filter_map(|c| warp::WarpJob::for_clip(c, &loader(&c.path)?, &map, sample_rate))
            .collect()
    }

    /// Full sequence for an engine at `sample_rate`: `warps` gives the render
    /// of each warped clip (`None`: not ready, the clip plays resampled).
    pub fn sequence_for(&self, sample_rate: f32, loader: SampleLoader, warps: WarpLookup) -> Sequence {
        let map = self.tempo_map();
        let n = self.channels.len();
        let patterns = self
            .patterns
            .iter()
            .map(|p| {
                let mut notes = match &p.groove {
                    Some(g) => p.notes.iter().map(|n| g.apply(n)).collect(),
                    None => p.notes.clone(),
                };
                notes.resize(n, Vec::new());
                let mut pat = Pattern { len: p.len, notes, ratchet_span: 0 };
                pat.sort();
                pat
            })
            .collect();
        Sequence {
            tempo: map.clone(),
            patterns,
            clips: self
                .clips
                .iter()
                .filter(|c| !c.muted)
                .map(|c| Clip { pattern: c.pattern, start: c.start, len: c.len, offset: c.offset })
                .collect(),
            mode: self.mode,
            song_len: self.song_len(),
            swing: self.swing,
            loop_range: self.loop_range,
            session: self.session_slots_live(),
            automation: self
                .automation
                .iter()
                .filter(|a| !a.points.is_empty())
                .map(|a| AutomationLane { target: a.target, scope: a.scope, points: a.points.clone() })
                .collect(),
            // The session plays its own audio clips, not the playlist's.
            audio_clips: if self.mode == PlayMode::Session {
                self.session_audio_live(sample_rate, loader, warps)
            } else {
                self.song_audio(sample_rate, loader, warps, &map)
            },
        }
    }

    /// Engine clips of the playlist's audio clips and comps (frozen tracks play their render).
    fn song_audio(&self, sample_rate: f32, loader: SampleLoader, warps: WarpLookup, map: &TempoMap) -> Vec<AudioClip> {
        self.audio_clips
            .iter()
            .filter(|c| !c.muted && self.frozen_track(c.track, loader).is_none())
            .filter_map(|c| {
                let sample = loader(&c.path)?;
                let warp = c.warp.then(|| ClipWarp {
                    map: c.warp_map(),
                    rendered: warp::WarpJob::for_clip(c, &sample, map, sample_rate).and_then(|job| warps(&job)),
                });
                Some(comp::engine_clip(c, sample, warp))
            })
            // Comps: their segments, crossfaded (takes play unwarped).
            .chain(self.take_groups.iter().filter(|g| self.frozen_track(g.track, loader).is_none()).flat_map(|g| g.pieces(map)).filter_map(|p| {
                let mut clip = comp::engine_clip(&p.clip, loader(&p.clip.path)?, None);
                (clip.xfade_in, clip.xfade_out) = (p.xfade_in, p.xfade_out);
                Some(clip)
            }))
            .collect()
    }

    // --- Automation ---

    pub fn automation_lane(&self, target: AutoTarget, scope: AutoScope) -> Option<&AutomationDef> {
        self.automation.iter().find(|a| a.target == target && a.scope == scope)
    }

    pub fn automation_lane_mut(&mut self, target: AutoTarget, scope: AutoScope) -> Option<&mut AutomationDef> {
        self.automation.iter_mut().find(|a| a.target == target && a.scope == scope)
    }

    /// Pose un point (courbe créée au besoin) ; un point au même tick prend la
    /// nouvelle valeur et garde sa tension. Renvoie l'indice du point.
    pub fn add_automation_point(&mut self, target: AutoTarget, scope: AutoScope, tick: u32, value: f32) -> usize {
        if self.automation_lane(target, scope).is_none() {
            self.automation.push(AutomationDef { target, scope, points: Vec::new() });
        }
        let points = &mut self.automation_lane_mut(target, scope).expect("courbe créée").points;
        let at = points.partition_point(|p| p.tick < tick);
        match points.get_mut(at) {
            Some(p) if p.tick == tick => p.value = value,
            _ => points.insert(at, AutoPoint { tick, value, curve: 0.0 }),
        }
        at
    }

    /// Retire le point posé à `tick` ; une courbe vidée disparaît.
    pub fn remove_automation_point(&mut self, target: AutoTarget, scope: AutoScope, tick: u32) -> Option<AutoPoint> {
        let i = self.automation.iter().position(|a| a.target == target && a.scope == scope)?;
        let points = &mut self.automation[i].points;
        let at = points.iter().position(|p| p.tick == tick)?;
        let removed = points.remove(at);
        if points.is_empty() {
            self.automation.remove(i);
        }
        Some(removed)
    }

    /// Removes the points of a lane from `from` to `to` (ticks, both
    /// included); an emptied lane disappears. True if any was removed.
    pub fn erase_automation(&mut self, target: AutoTarget, scope: AutoScope, from: u32, to: u32) -> bool {
        let Some(i) = self.automation.iter().position(|a| a.target == target && a.scope == scope) else { return false };
        let points = &mut self.automation[i].points;
        let before = points.len();
        points.retain(|p| p.tick < from || p.tick > to);
        let removed = points.len() != before;
        if points.is_empty() {
            self.automation.remove(i);
        }
        removed
    }

    /// Ordre de traitement du mixer, ou `None` si le routage boucle.
    pub fn routing_order(&self) -> Option<Box<[usize]>> {
        let edges: Vec<Vec<usize>> = self
            .tracks
            .iter()
            .enumerate()
            .map(|(i, t)| {
                if i == MASTER {
                    return Vec::new();
                }
                let mut out = vec![if t.output == i { MASTER } else { t.output }];
                for &(target, amount) in &t.sends {
                    if amount > 0.0 && target != i && !out.contains(&target) {
                        out.push(target);
                    }
                }
                out
            })
            .collect();
        // Une piste clé passe avant la piste qui l'écoute (une clé hors limites est refusée).
        let mut edges = edges;
        for (i, t) in self.tracks.iter().enumerate() {
            for key in t.inserts.iter().flatten().filter_map(|fx| fx.sidechain) {
                edges.get_mut(key)?.push(i);
            }
        }
        routing_order(&edges)
    }

    /// Crée l'instrument d'un canal, réglé et prêt à envoyer au moteur.
    ///
    /// Un plugin CLAP ou VST 3 introuvable ou défaillant est remplacé par Pulse (le projet
    /// garde son identifiant d'origine).
    pub fn make_instrument(def: &InstrumentDef, sample_rate: f32, loader: SampleLoader) -> Box<dyn Instrument> {
        let created = if def.kind.starts_with(ponzik_clap::KIND_PREFIX) {
            ponzik_clap::instrument_from_kind(&def.kind).map_err(|e| eprintln!("CLAP : {e}")).ok()
        } else if def.kind.starts_with(ponzik_vst3::KIND_PREFIX) {
            ponzik_vst3::instrument_from_kind(&def.kind).map_err(|e| eprintln!("VST 3 : {e}")).ok()
        } else {
            registry::create_instrument(&def.kind)
        };
        let mut inst = created.or_else(|| registry::create_instrument("pulse")).expect("le registre contient Pulse");
        // The plugin's own state first, before activation; the saved values
        // (newer, when an undo step restores an older state) come after.
        if let Some(state) = &def.state
            && inst.kind() == def.kind
            && let Err(e) = inst.load_state(state)
        {
            eprintln!("{}: {e}", def.kind);
        }
        inst.set_sample_rate(sample_rate);
        for &(id, v) in &def.params {
            inst.set_param(id, v);
        }
        if let Some(path) = &def.sample
            && let Some(sample) = loader(path)
        {
            inst.set_sample(sample);
        }
        for (pad, layers) in def.pads.iter().enumerate() {
            for (layer, path) in layers.iter().enumerate() {
                if let Some(sample) = loader(path) {
                    inst.set_pad_sample(pad, layer, Some(sample));
                }
            }
        }
        inst
    }

    /// Like [`make_instrument`](Self::make_instrument), a sampler in tempo
    /// sync getting its sample stretched to the project tempo.
    pub fn make_synced_instrument(&self, def: &InstrumentDef, sample_rate: f32, loader: SampleLoader) -> Box<dyn Instrument> {
        if sync_key(def, self.bpm).is_none() {
            return Self::make_instrument(def, sample_rate, loader);
        }
        let bpm = self.bpm;
        let synced = |path: &str| loader(path).map(|s| synced_sample(def, s, bpm));
        Self::make_instrument(def, sample_rate, &synced)
    }

    /// `None` si l'identifiant est inconnu ou si le plugin CLAP ou VST 3 ne se charge pas.
    pub fn make_effect(def: &EffectDef, sample_rate: f32, bpm: f32) -> Option<Box<dyn Effect>> {
        if def.kind == rack::RACK {
            return Some(rack::make_rack(def, sample_rate, bpm));
        }
        let mut fx = if def.kind.starts_with(ponzik_clap::KIND_PREFIX) {
            ponzik_clap::effect_from_kind(&def.kind).map_err(|e| eprintln!("CLAP : {e}")).ok()?
        } else if def.kind.starts_with(ponzik_vst3::KIND_PREFIX) {
            ponzik_vst3::effect_from_kind(&def.kind).map_err(|e| eprintln!("VST 3 : {e}")).ok()?
        } else {
            registry::create_effect(&def.kind)?
        };
        if let Some(state) = &def.state
            && let Err(e) = fx.load_state(state)
        {
            eprintln!("{}: {e}", def.kind);
        }
        fx.set_sample_rate(sample_rate);
        fx.set_tempo(bpm);
        for &(id, v) in &def.params {
            fx.set_param(id, v);
        }
        Some(fx)
    }

    /// `None` si l'identifiant est inconnu.
    pub fn make_note_fx(def: &NoteFxDef, sample_rate: f32, bpm: f32) -> Option<Box<dyn NoteFx>> {
        let mut fx = registry::create_note_fx(&def.kind)?;
        fx.set_sample_rate(sample_rate);
        fx.set_tempo(bpm);
        for &(id, v) in &def.params {
            fx.set_param(id, v);
        }
        Some(fx)
    }

    pub fn make_channel(&self, idx: usize, sample_rate: f32, loader: SampleLoader) -> Channel {
        let c = &self.channels[idx];
        if !self.channel_live(idx, loader) {
            // Frozen, by itself or with its track: a placeholder that plays its
            // render (nothing at all when the track's render covers it).
            let mut ch = Channel::new(c.name.clone(), ponzik_engine::silent_instrument())
                .with_gain(c.gain)
                .with_pan(c.pan)
                .with_track(c.track);
            ch.muted = c.muted;
            if self.frozen_track(c.track, loader).is_none() {
                ch.frozen = self.frozen_channel(idx, loader).map(|s| Box::new(Frozen::new(s)));
            }
            return ch;
        }
        let mut ch = Channel::new(c.name.clone(), self.make_synced_instrument(&c.instrument, sample_rate, loader))
            .with_gain(c.gain)
            .with_pan(c.pan)
            .with_track(c.track)
            .with_porta(c.porta);
        ch.muted = c.muted;
        for (slot, def) in c.note_fx.iter().enumerate().take(NOTE_FX_SLOTS) {
            ch.note_fx[slot] = def.as_ref().and_then(|d| Self::make_note_fx(d, sample_rate, self.bpm));
        }
        ch.mods = self.make_channel_mods(idx);
        ch
    }

    /// Moteur complet dans l'état du projet, warped clips rendered now.
    pub fn build_engine(&self, sample_rate: f32, loader: SampleLoader) -> (Engine, EngineHandle) {
        self.build_engine_with(sample_rate, loader, &render_warps)
    }

    /// Moteur complet ; `warps` gives the renders of the warped clips.
    pub fn build_engine_with(&self, sample_rate: f32, loader: SampleLoader, warps: WarpLookup) -> (Engine, EngineHandle) {
        let channels = (0..self.channels.len()).map(|i| self.make_channel(i, sample_rate, loader)).collect();
        let (mut engine, handle) = Engine::new(sample_rate, channels, self.sequence_for(sample_rate, loader, warps));
        engine.set_track_count(self.tracks.len());
        let mixer = engine.mixer_mut();
        for (i, def) in self.tracks.iter().enumerate().take(mixer.len()) {
            let t = &mut mixer.tracks[i];
            t.gain = def.gain;
            t.pan = def.pan;
            t.muted = def.muted;
            t.output = def.output;
            t.sends = def.sends;
            t.vca = def.vca;
            if let Some(sample) = self.frozen_track(i, loader) {
                // Frozen: its render replaces its input and inserts, which stay unloaded.
                t.frozen = Some(Box::new(Frozen::new(sample)));
                continue;
            }
            for (slot, fx) in def.inserts.iter().enumerate() {
                t.inserts[slot] = fx.as_ref().and_then(|d| Self::make_effect(d, sample_rate, self.bpm));
                t.bypass[slot] = fx.as_ref().is_some_and(|d| d.bypass);
                t.sidechain[slot] = fx.as_ref().and_then(|d| d.sidechain);
            }
            t.mods = self.make_track_mods(i);
        }
        if let Some(order) = self.routing_order() {
            mixer.order = order;
        }
        (engine, handle)
    }

    // --- Édition des notes ---

    fn pattern_notes(&mut self, pattern: usize, channel: usize) -> &mut Vec<Note> {
        let n = self.channels.len();
        let p = &mut self.patterns[pattern];
        if p.notes.len() < n {
            p.notes.resize(n, Vec::new());
        }
        &mut p.notes[channel]
    }

    /// Vrai si une note commence exactement sur ce pas.
    pub fn step_on(&self, pattern: usize, channel: usize, step: u32) -> Option<Note> {
        let start = step * TICKS_PER_STEP;
        self.patterns.get(pattern)?.notes.get(channel)?.iter().find(|n| n.start == start).copied()
    }

    /// Allume ou éteint un pas du step sequencer ; renvoie vrai si quelque chose a changé.
    pub fn set_step(&mut self, pattern: usize, channel: usize, step: u32, on: bool) -> bool {
        let start = step * TICKS_PER_STEP;
        let key = self.channels[channel].root_note;
        let notes = self.pattern_notes(pattern, channel);
        let existing = notes.iter().position(|n| n.start == start);
        match (existing, on) {
            (Some(_), true) | (None, false) => false,
            (Some(i), false) => {
                notes.remove(i);
                true
            }
            (None, true) => {
                let at = notes.partition_point(|n| n.start < start);
                notes.insert(at, Note { start, len: TICKS_PER_STEP, key, vel: 0.8, ..Note::DEFAULT });
                true
            }
        }
    }

    pub fn add_note(&mut self, pattern: usize, channel: usize, note: Note) {
        let notes = self.pattern_notes(pattern, channel);
        let at = notes.partition_point(|n| (n.start, n.key) < (note.start, note.key));
        notes.insert(at, note);
    }

    pub fn remove_note(&mut self, pattern: usize, channel: usize, index: usize) -> Option<Note> {
        let notes = self.pattern_notes(pattern, channel);
        (index < notes.len()).then(|| notes.remove(index))
    }

    pub fn sort_notes(&mut self, pattern: usize, channel: usize) {
        self.pattern_notes(pattern, channel).sort_by_key(|n| (n.start, n.key));
    }

    /// Ajoute un canal et sa piste de mixer libre suivante (a new one at the
    /// end when none is free).
    pub fn add_channel(&mut self, name: impl Into<String>, instrument: InstrumentDef, root_note: u8) -> usize {
        let used: Vec<usize> = self.channels.iter().map(|c| c.track).collect();
        let free = |t: &usize| !used.contains(t) && self.tracks[*t].kind == TrackKind::Normal;
        let track = (1..self.tracks.len()).find(free).or_else(|| self.add_track()).unwrap_or(1);
        let name = name.into();
        if is_default_track_name(&self.tracks[track].name) {
            self.tracks[track].name = name.clone();
        }
        self.channels.push(ChannelDef {
            name,
            instrument,
            note_fx: Vec::new(),
            gain: 0.8,
            pan: 0.0,
            muted: false,
            track,
            root_note,
            color: 0,
            modulators: Vec::new(),
            frozen: None,
            porta: 0.0,
        });
        for p in &mut self.patterns {
            p.notes.resize(self.channels.len(), Vec::new());
        }
        self.channels.len() - 1
    }

    pub fn remove_channel(&mut self, idx: usize) {
        if idx >= self.channels.len() {
            return;
        }
        self.channels.remove(idx);
        for p in &mut self.patterns {
            if idx < p.notes.len() {
                p.notes.remove(idx);
            }
        }
        // Les courbes du canal disparaissent, celles des suivants suivent leur canal.
        self.automation.retain_mut(|a| {
            let channel = match &mut a.target {
                AutoTarget::InstrumentParam { channel, .. }
                | AutoTarget::ChannelGain(channel)
                | AutoTarget::ChannelPan(channel)
                | AutoTarget::PitchBend(channel)
                | AutoTarget::Cc { channel, .. } => channel,
                _ => return true,
            };
            if *channel == idx {
                return false;
            }
            if *channel > idx {
                *channel -= 1;
            }
            true
        });
    }

    /// Nom libre dérivé de `base` : « Groove » donne « Groove 2 », « Pattern 3 »
    /// donne « Pattern 4 » (ou le suivant libre).
    pub fn unique_pattern_name(&self, base: &str) -> String {
        let taken = |n: &str| self.patterns.iter().any(|p| p.name == n);
        let (stem, mut n) = match base.rsplit_once(' ') {
            Some((stem, num)) if !stem.is_empty() && num.parse::<u32>().is_ok() => (stem, num.parse::<u32>().unwrap_or(1)),
            _ => (base, 1),
        };
        loop {
            n += 1;
            let name = format!("{stem} {n}");
            if !taken(&name) {
                return name;
            }
        }
    }

    /// « Make unique » : le clip reçoit sa propre copie du pattern (notes,
    /// longueur, couleur), qu'on peut modifier sans toucher aux autres clips.
    /// Renvoie l'indice du nouveau pattern.
    pub fn make_unique(&mut self, clip: usize) -> Option<usize> {
        let src = self.clips.get(clip)?.pattern;
        let mut copy = self.patterns.get(src)?.clone();
        copy.name = self.unique_pattern_name(&copy.name);
        self.patterns.push(copy);
        let p = self.patterns.len() - 1;
        self.clips[clip].pattern = p;
        Some(p)
    }


    /// Vide la plage `[a, b)` de la playlist : les clips qui la chevauchent sont
    /// raccourcis, ou coupés en deux s'ils la traversent.
    pub fn clear_time(&mut self, a: u32, b: u32) {
        if b <= a {
            return;
        }
        let mut clips = Vec::with_capacity(self.clips.len());
        for c in &self.clips {
            let end = c.start.saturating_add(c.len);
            if end <= a || c.start >= b {
                clips.push(*c);
                continue;
            }
            if c.start < a {
                clips.push(ClipDef { len: a - c.start, ..*c });
            }
            if end > b {
                clips.push(ClipDef { start: b, len: end - b, offset: c.offset + (b - c.start), ..*c });
            }
        }
        self.clips = clips;
        let map = self.tempo_map();
        let mut audio = Vec::with_capacity(self.audio_clips.len());
        for c in &self.audio_clips {
            let end = c.start.saturating_add(c.len);
            if end <= a || c.start >= b {
                audio.push(c.clone());
                continue;
            }
            if c.start < a {
                audio.push(AudioClipDef { len: a - c.start, fade_out: 0.0, ..c.clone() });
            }
            if end > b {
                let offset = c.offset_at(b as f64, &map);
                audio.push(AudioClipDef { start: b, len: end - b, offset, fade_in: 0.0, ..c.clone() });
            }
        }
        self.audio_clips = audio;
        // Take groups go whole or stay whole.
        self.take_groups.retain(|g| {
            let (s, e) = g.span();
            s < a || e > b
        });
    }

    /// Recopie tout ce que contient la plage `[a, b)` de la playlist juste
    /// après elle, dans `[b, 2b − a)`, à la place de ce qui s'y trouvait : clips
    /// de pattern et clips audio (coupés aux bords de la plage) et points
    /// d'automation du morceau. Renvoie le nombre de clips copiés.
    pub fn duplicate_time(&mut self, a: u32, b: u32) -> usize {
        if b <= a {
            return 0;
        }
        let span = b - a;
        let cut = |start: u32, len: u32| {
            let (s, e) = (start.max(a), start.saturating_add(len).min(b));
            (s < e).then(|| (s, e - s, s - start))
        };
        let clips: Vec<ClipDef> = self
            .clips
            .iter()
            .filter_map(|c| {
                let (start, len, skip) = cut(c.start, c.len)?;
                Some(ClipDef { start: start + span, len, offset: c.offset + skip, ..*c })
            })
            .collect();
        let map = self.tempo_map();
        let audio: Vec<AudioClipDef> = self
            .audio_clips
            .iter()
            .filter_map(|c| {
                let (start, len, skip) = cut(c.start, c.len)?;
                // Une coupe à l'intérieur du clip n'a pas de fondu d'entrée ou de sortie.
                let fade_in = if skip == 0 { c.fade_in } else { 0.0 };
                let fade_out = if start + len == c.start + c.len { c.fade_out } else { 0.0 };
                let offset = c.offset_at((c.start + skip) as f64, &map);
                Some(AudioClipDef { start: start + span, len, offset, fade_in, fade_out, ..c.clone() })
            })
            .collect();
        let groups: Vec<TakeGroup> = self
            .take_groups
            .iter()
            .filter(|g| g.span().0 >= a && g.span().1 <= b)
            .map(|g| {
                let mut g = g.clone();
                g.shift(span as i64);
                g
            })
            .collect();
        let copied = clips.len() + audio.len() + groups.len();
        self.clear_time(b, b + span);
        self.clips.extend(clips);
        self.audio_clips.extend(audio);
        self.take_groups.extend(groups);

        let end = b + span;
        for lane in self.automation.iter_mut().filter(|l| l.scope == AutoScope::Song && !l.points.is_empty()) {
            let at = |t: u32| lane.value_at(t as f64).unwrap_or(0.0);
            let (head, last, tail) = (at(a), at(b - 1), at(end));
            let mut copy: Vec<AutoPoint> = lane
                .points
                .iter()
                .filter(|p| p.tick >= a && p.tick < b)
                .map(|p| AutoPoint { tick: p.tick + span, ..*p })
                .collect();
            if copy.first().is_none_or(|p| p.tick != b) {
                copy.insert(0, AutoPoint { tick: b, value: head, curve: 0.0 });
            }
            // La copie s'arrête sur la valeur de la fin de la plage, un tick avant
            // de retrouver la courbe d'origine.
            if copy.last().is_some_and(|p| p.tick < end - 1) {
                copy.push(AutoPoint { tick: end - 1, value: last, curve: 0.0 });
            }
            let has_tail = lane.points.iter().any(|p| p.tick == end);
            lane.points.retain(|p| p.tick < b || p.tick >= end);
            if !has_tail {
                copy.push(AutoPoint { tick: end, value: tail, curve: 0.0 });
            }
            let at = lane.points.partition_point(|p| p.tick < b);
            lane.points.splice(at..at, copy);
        }
        copied
    }

    // --- Bus et VCA ---

    /// Vrai si la piste sert à quelque chose : canal, clip audio, effet, nom
    /// donné, rôle de bus ou de VCA, ou destination d'une autre piste.
    pub fn track_in_use(&self, t: usize) -> bool {
        let Some(tr) = self.tracks.get(t) else { return false };
        t == MASTER
            || tr.kind != TrackKind::Normal
            || !is_default_track_name(&tr.name)
            || tr.inserts.iter().any(Option::is_some)
            || self.channels.iter().any(|c| c.track == t)
            || self.audio_clips.iter().any(|c| c.track == t)
            || self.take_groups.iter().any(|g| g.track == t)
            || self.tracks.iter().enumerate().any(|(i, x)| {
                i != t && (x.output == t || x.vca == Some(t) || x.sends.iter().any(|s| s.0 == t && s.1 > 0.0))
            })
    }

    /// Last unused track, if any.
    pub fn free_track(&self) -> Option<usize> {
        (1..self.tracks.len()).rev().find(|&t| !self.track_in_use(t))
    }

    /// Pistes qui sortent dans `bus`.
    pub fn bus_members(&self, bus: usize) -> Vec<usize> {
        (1..self.tracks.len()).filter(|&t| t != bus && self.tracks[t].output == bus).collect()
    }

    /// Pistes pilotées par le VCA `vca`.
    pub fn vca_members(&self, vca: usize) -> Vec<usize> {
        (1..self.tracks.len()).filter(|&t| self.tracks[t].vca == Some(vca)).collect()
    }

    /// Premier nom libre de la forme « Bus 1 », « Bus 2 »…
    fn numbered_name(&self, base: &str) -> String {
        (1..).map(|n| format!("{base} {n}")).find(|name| self.tracks.iter().all(|t| &t.name != name)).unwrap_or_default()
    }

    /// A new track at the end for a bus or a VCA, in its own color.
    fn claim_track(&mut self, kind: TrackKind, name: String) -> Option<usize> {
        let t = self.add_track()?;
        // Première couleur pas encore prise, sinon chacune à son tour.
        let others = self.tracks.iter().filter(|t| t.kind != TrackKind::Normal).count();
        let color = BUS_COLORS
            .iter()
            .copied()
            .find(|&c| self.tracks.iter().all(|t| t.color != c))
            .unwrap_or(BUS_COLORS[others % BUS_COLORS.len()]);
        let tr = &mut self.tracks[t];
        tr.name = name;
        tr.kind = kind;
        tr.color = color;
        Some(t)
    }

    /// Crée un bus et y fait sortir `members` (le master, le bus et les pistes
    /// qui boucleraient sont ignorés). Si toutes les pistes sortaient dans un
    /// même bus, le nouveau s'y range à leur place. `None` si plus aucune piste
    /// n'est libre.
    pub fn add_bus(&mut self, members: &[usize]) -> Option<usize> {
        let members: Vec<usize> = members.iter().copied().filter(|&m| m != MASTER && m < self.tracks.len()).collect();
        let name = self.numbered_name(tr("Bus"));
        let bus = self.claim_track(TrackKind::Bus, name)?;
        let mut outputs = members.iter().map(|&m| self.tracks[m].output);
        if let Some(first) = outputs.next()
            && first != MASTER
            && first != bus
            && outputs.all(|o| o == first)
        {
            self.tracks[bus].output = first;
        }
        for &m in &members {
            self.route_into(m, bus);
        }
        Some(bus)
    }

    /// Fait sortir `track` dans `bus`, sauf si cela crée une boucle.
    pub fn route_into(&mut self, track: usize, bus: usize) -> bool {
        if track == MASTER || track == bus || track >= self.tracks.len() || bus >= self.tracks.len() {
            return false;
        }
        let old = self.tracks[track].output;
        self.tracks[track].output = bus;
        if self.routing_order().is_none() {
            self.tracks[track].output = old;
            return false;
        }
        true
    }

    /// Bus de retour d'effet (`RETURN_EFFECTS`) ; `from` y envoie un peu de son signal.
    pub fn add_return(&mut self, effect: &str, from: Option<usize>) -> Option<usize> {
        let &(kind, name, params) = RETURN_EFFECTS.iter().find(|e| e.0 == effect)?;
        let name = tr(name);
        let name = if self.tracks.iter().any(|t| t.name == name) { self.numbered_name(name) } else { name.into() };
        let bus = self.claim_track(TrackKind::Bus, name)?;
        let mut fx = EffectDef::new(kind);
        for &(id, v) in params {
            fx.set(id, v);
        }
        self.tracks[bus].inserts[0] = Some(fx);
        if let Some(src) = from.filter(|&t| t != MASTER && t != bus && t < self.tracks.len()) {
            let sends = &mut self.tracks[src].sends;
            if let Some(slot) = sends.iter().position(|s| s.1 <= 0.0) {
                sends[slot] = (bus, 0.3);
            }
        }
        Some(bus)
    }

    /// Crée un VCA qui pilote `members` (le master et les VCA sont ignorés).
    pub fn add_vca(&mut self, members: &[usize]) -> Option<usize> {
        let name = self.numbered_name(tr("VCA"));
        let vca = self.claim_track(TrackKind::Vca, name)?;
        for &m in members {
            if m != MASTER && m != vca && m < self.tracks.len() && self.tracks[m].kind != TrackKind::Vca {
                self.tracks[m].vca = Some(vca);
            }
        }
        Some(vca)
    }

    /// Défait un bus ou un VCA : ses pistes sortent là où il sortait, les envois
    /// et les clés qui le visaient sont retirés, et la piste redevient libre.
    pub fn dissolve(&mut self, t: usize) {
        if t == MASTER || t >= self.tracks.len() || self.tracks[t].kind == TrackKind::Normal {
            return;
        }
        let output = self.tracks[t].output;
        for (i, tr) in self.tracks.iter_mut().enumerate() {
            if i != t && tr.output == t {
                tr.output = output;
            }
            if tr.vca == Some(t) {
                tr.vca = None;
            }
            for s in &mut tr.sends {
                if s.0 == t {
                    *s = (MASTER, 0.0);
                }
            }
            for fx in tr.inserts.iter_mut().flatten() {
                if fx.sidechain == Some(t) {
                    fx.sidechain = None;
                }
            }
        }
        self.automation.retain(|a| !matches!(a.target, AutoTarget::TrackGain(x) | AutoTarget::TrackPan(x) if x == t));
        self.automation.retain(|a| !matches!(a.target, AutoTarget::EffectParam { track, .. } if track == t));
        let id = self.tracks[t].id;
        self.tracks[t] = TrackDef { id, ..TrackDef::new(default_track_name(t)) };
    }

    pub fn add_pattern(&mut self) -> usize {
        let n = self.patterns.len() + 1;
        self.patterns.push(PatternDef {
            name: tr_fmt!("Pattern {}", n),
            len: TICKS_PER_BAR,
            notes: vec![Vec::new(); self.channels.len()],
            color: 0,
            groove: None,
        });
        self.patterns.len() - 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ponzik_engine::Command;

    fn no_samples(_: &str) -> Option<Arc<Sample>> {
        None
    }

    #[test]
    fn warped_clips_count_the_file_in_beats() {
        let map = TempoMap::constant(120.0);
        let plain = AudioClipDef { start: 96, offset: 0.5, ..Default::default() };
        let warped = AudioClipDef { warp: true, bpm: 60.0, ..plain.clone() };
        // One beat later: half a second of the file unwarped, a whole one at 60 BPM.
        assert!((plain.offset_at(192.0, &map) - 1.0).abs() < 1e-9);
        assert!((warped.offset_at(192.0, &map) - 1.5).abs() < 1e-9);
        // A 4.5 s file from 0.5 s: 8 beats at 120 BPM, 4 at 60.
        assert_eq!(plain.natural_len(4.5, &map), 8 * PPQ);
        assert_eq!(warped.natural_len(4.5, &map), 4 * PPQ);
        // Cutting a warped clip keeps the right part on the same audio.
        let mut p = Project::empty();
        p.audio_clips.push(AudioClipDef { len: 4 * PPQ, ..warped });
        p.insert_gap(96 + 2 * PPQ, PPQ);
        let right = p.audio_clips.iter().find(|c| c.start == 96 + 3 * PPQ).expect("right part");
        assert!((right.offset - 2.5).abs() < 1e-9, "{}", right.offset);
    }

    #[test]
    fn a_sampler_in_tempo_sync_gets_its_loop_stretched_to_the_tempo() {
        use ponzik_engine::instruments::{SAMPLER_BEATS, SAMPLER_SYNC};
        let left: Vec<f32> = (0..96_000).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
        let loop_ = Arc::new(Sample { name: "loop".into(), sample_rate: 48_000.0, right: left.clone(), left, markers: Vec::new() });
        let mut def = InstrumentDef::new("sampler");
        assert_eq!(sync_key(&def, 60.0), None);
        assert!(Arc::ptr_eq(&synced_sample(&def, loop_.clone(), 60.0), &loop_));
        // Four beats in 2 s (120 BPM), played at 60 BPM: 4 s.
        def.set(SAMPLER_SYNC, 1.0);
        def.set(SAMPLER_BEATS, 4.0);
        assert!(sync_key(&def, 60.0).is_some());
        assert_ne!(sync_key(&def, 60.0), sync_key(&def, 90.0));
        let stretched = synced_sample(&def, loop_.clone(), 60.0);
        assert!((stretched.duration() - 4.0).abs() < 0.01, "{}", stretched.duration());
        // At its own tempo: as is.
        assert!((synced_sample(&def, loop_, 120.0).duration() - 2.0).abs() < 0.01);
    }

    #[test]
    fn looped_clips_mark_where_the_file_starts_over() {
        let map = TempoMap::constant(120.0);
        // A 2 s loop (one bar at 120 BPM) stretched over four bars.
        let plain = AudioClipDef { start: 96, len: 16 * PPQ, looped: true, ..Default::default() };
        let starts = plain.loop_starts(2.0, &map, 64);
        assert_eq!(starts.iter().map(|&t| t.round() as u32).collect::<Vec<_>>(), [96 + 4 * PPQ, 96 + 8 * PPQ, 96 + 12 * PPQ]);
        assert_eq!(plain.loop_period(2.0, &map), 4 * PPQ);
        // Warped at 60 BPM: the file lasts two beats, wherever the song tempo is.
        let warped = AudioClipDef { warp: true, bpm: 60.0, offset: 1.0, ..plain.clone() };
        let starts = warped.loop_starts(2.0, &map, 64);
        assert_eq!(starts.first().map(|t| t.round() as u32), Some(96 + PPQ));
        assert_eq!(starts.len(), 8);
        assert_eq!(warped.loop_period(2.0, &map), 2 * PPQ);
        // Not looped: no marks.
        assert!(AudioClipDef { looped: false, ..plain }.loop_starts(2.0, &map, 64).is_empty());
    }

    #[test]
    fn drum_pads_get_their_pad_samples() {
        let tone: Vec<f32> = (0..4_800).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
        let sample = Arc::new(Sample { name: "t".into(), sample_rate: 48_000.0, left: tone.clone(), right: tone, markers: Vec::new() });
        let loader = |path: &str| (path == "snare.wav").then(|| sample.clone());
        let mut def = InstrumentDef::new("drum_pads");
        def.pads = vec![Vec::new(), Vec::new(), vec!["snare.wav".into(), "missing.wav".into()]];
        assert_eq!(def.sample_paths().count(), 2);
        let mut inst = Project::make_instrument(&def, 48_000.0, &loader);
        let (mut l, mut r) = (vec![0.0; 1_024], vec![0.0; 1_024]);
        inst.note_on(36, 1.0);
        inst.render(&mut l, &mut r);
        assert!(l.iter().all(|&s| s == 0.0), "the kick pad is empty");
        inst.note_on(38, 1.0);
        inst.render(&mut l, &mut r);
        assert!(l.iter().any(|s| s.abs() > 0.1), "the snare pad plays");
    }

    #[test]
    fn demo_builds_and_plays() {
        let p = demo_project();
        let (mut engine, mut handle) = p.build_engine(48_000.0, &no_samples);
        handle.send(Command::Play).unwrap();
        let mut buf = vec![0.0f32; 2 * 48_000];
        for chunk in buf.chunks_mut(1_024) {
            engine.process(chunk, 2);
        }
        let peak = buf.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak > 0.05 && peak <= 1.0, "{peak}");
    }

    #[test]
    fn missing_clap_plugin_falls_back() {
        let kind = "clap:/introuvable/x.clap#com.exemple.synth";
        let inst = Project::make_instrument(&InstrumentDef::new(kind), 48_000.0, &no_samples);
        assert_eq!(inst.kind(), "pulse");
        assert!(Project::make_effect(&EffectDef::new(kind), 48_000.0, 120.0).is_none());
    }

    #[test]
    fn missing_vst3_plugin_falls_back() {
        let kind = "vst3:/introuvable/x.vst3#0123456789ABCDEF0123456789ABCDEF";
        let inst = Project::make_instrument(&InstrumentDef::new(kind), 48_000.0, &no_samples);
        assert_eq!(inst.kind(), "pulse");
        assert!(Project::make_effect(&EffectDef::new(kind), 48_000.0, 120.0).is_none());
    }

    #[test]
    fn steps_toggle_notes() {
        let mut p = Project::empty();
        let ch = p.add_channel("Kick", InstrumentDef::new("drumsynth"), 60);
        assert!(p.set_step(0, ch, 4, true));
        assert!(!p.set_step(0, ch, 4, true));
        assert_eq!(p.step_on(0, ch, 4).map(|n| n.start), Some(4 * TICKS_PER_STEP));
        assert!(p.set_step(0, ch, 4, false));
        assert!(p.step_on(0, ch, 4).is_none());
        assert_eq!(p.channels[ch].track, 1);
    }

    #[test]
    fn song_length_and_routing() {
        let mut p = Project::empty();
        assert_eq!(p.song_len(), TICKS_PER_BAR);
        p.clips.push(ClipDef { pattern: 0, lane: 0, start: TICKS_PER_BAR * 2, len: TICKS_PER_BAR / 2, offset: 0, muted: false });
        assert_eq!(p.song_len(), TICKS_PER_BAR * 3);
        assert!(p.routing_order().is_some());
        p.tracks[1].output = 2;
        p.tracks[2].output = 1;
        assert!(p.routing_order().is_none());
    }

    #[test]
    fn make_unique_gives_the_clip_its_own_pattern() {
        let mut p = Project::empty();
        let ch = p.add_channel("Kick", InstrumentDef::new("drumsynth"), 60);
        p.set_step(0, ch, 0, true);
        p.patterns[0].color = 0x123456;
        let clip = ClipDef { pattern: 0, lane: 0, start: 0, len: TICKS_PER_BAR, offset: 0, muted: false };
        p.clips.extend([clip, ClipDef { start: TICKS_PER_BAR, ..clip }]);
        assert_eq!(p.make_unique(1), Some(1));
        assert_eq!(p.clips[1].pattern, 1);
        assert_eq!(p.clips[0].pattern, 0);
        assert_eq!(p.patterns[1].name, "Pattern 2");
        assert_eq!(p.patterns[1].notes, p.patterns[0].notes);
        assert_eq!(p.patterns[1].color, 0x123456);
        // Une modification du nouveau pattern ne touche pas l'ancien.
        p.set_step(1, ch, 4, true);
        assert!(p.step_on(0, ch, 4).is_none());
        assert_eq!(p.make_unique(9), None);
        assert_eq!(p.unique_pattern_name("Groove"), "Groove 2");
        assert_eq!(p.unique_pattern_name("Pattern 1"), "Pattern 3");
    }

    #[test]
    fn duplicate_time_copies_clips_audio_and_automation() {
        let mut p = Project::empty();
        p.bpm = 120.0;
        let bar = TICKS_PER_BAR;
        // Un clip dans la plage, un à cheval sur son début, un après.
        p.clips.push(ClipDef { pattern: 0, lane: 0, start: bar, len: bar, offset: 0, muted: false });
        p.clips.push(ClipDef { pattern: 0, lane: 1, start: 0, len: 2 * bar, offset: 0, muted: false });
        p.clips.push(ClipDef { pattern: 0, lane: 2, start: 3 * bar, len: bar, offset: 0, muted: false });
        // Celui-ci traverse la destination : il est coupé autour de la copie.
        p.clips.push(ClipDef { pattern: 0, lane: 4, start: bar + 96, len: 3 * bar, offset: 0, muted: false });
        p.audio_clips.push(AudioClipDef {
            path: "a.wav".into(),
            lane: 3,
            track: 1,
            start: bar / 2,
            len: bar,
            offset: 0.0,
            gain: 1.0,
            fade_in: 0.01,
            fade_out: 0.01,
            take: 0,
            muted: false,
            ..Default::default()
        });
        let gain = AutoTarget::TrackGain(1);
        p.add_automation_point(gain, AutoScope::Song, 0, 0.0);
        p.add_automation_point(gain, AutoScope::Song, bar + 96, 1.0);
        p.add_automation_point(gain, AutoScope::Song, 2 * bar + 96, 0.2);
        p.add_automation_point(gain, AutoScope::Song, 4 * bar, 0.5);
        let before = p.automation[0].clone();

        assert_eq!(p.duplicate_time(bar, 2 * bar), 4);
        let lane4: Vec<_> = p.clips.iter().filter(|c| c.lane == 4).map(|c| (c.start, c.len, c.offset)).collect();
        assert_eq!(lane4, [(bar + 96, bar - 96, 0), (3 * bar, bar + 96, 2 * bar - 96), (2 * bar + 96, bar - 96, 0)]);
        assert!(p.clips.contains(&ClipDef { pattern: 0, lane: 0, start: 2 * bar, len: bar, offset: 0, muted: false }));
        assert!(p.clips.contains(&ClipDef { pattern: 0, lane: 1, start: 2 * bar, len: bar, offset: bar, muted: false }));
        assert!(p.clips.contains(&ClipDef { pattern: 0, lane: 2, start: 3 * bar, len: bar, offset: 0, muted: false }));
        // Le clip audio commence avant la plage : la copie saute une demi-mesure (1 s à 120 BPM).
        let a = p.audio_clips.last().unwrap();
        assert_eq!((a.start, a.len, a.lane), (2 * bar, bar / 2, 3));
        assert!((a.offset - 1.0).abs() < 1e-9);
        assert_eq!((a.fade_in, a.fade_out), (0.0, 0.01));

        // La courbe de la destination reprend celle de la plage, puis retombe sur l'original.
        let lane = &p.automation[0];
        for k in 0..bar {
            let (src, dst) = (before.value_at((bar + k) as f64), lane.value_at((2 * bar + k) as f64));
            assert!((src.unwrap() - dst.unwrap()).abs() < 1e-6, "{k}");
        }
        for t in [0, bar / 2, bar + 7, 3 * bar, 3 * bar + 50, 4 * bar, 5 * bar] {
            assert_eq!(lane.value_at(t as f64), before.value_at(t as f64), "{t}");
        }
        assert!(lane.points.windows(2).all(|w| w[0].tick < w[1].tick));
        // Plage vide ou à l'envers : rien.
        assert_eq!(p.duplicate_time(5, 5), 0);
    }

    #[test]
    fn clear_time_trims_and_splits_audio() {
        let mut p = Project::empty();
        p.bpm = 120.0;
        let bar = TICKS_PER_BAR;
        let clip = AudioClipDef {
            path: "a.wav".into(),
            lane: 0,
            track: 1,
            start: 0,
            len: 3 * bar,
            offset: 0.5,
            gain: 1.0,
            fade_in: 0.01,
            fade_out: 0.02,
            take: 0,
            muted: false,
            ..Default::default()
        };
        p.audio_clips.push(clip);
        p.clear_time(bar, 2 * bar);
        let parts: Vec<_> = p.audio_clips.iter().map(|c| (c.start, c.len, c.offset, c.fade_in, c.fade_out)).collect();
        // Une mesure = 2 s à 120 BPM.
        assert_eq!(parts, [(0, bar, 0.5, 0.01, 0.0), (2 * bar, bar, 4.5, 0.0, 0.02)]);
    }

    fn keyed(kind: &str, key: usize) -> Option<EffectDef> {
        Some(EffectDef { sidechain: Some(key), ..EffectDef::new(kind) })
    }

    #[test]
    fn sidechain_orders_and_rejects_cycles() {
        let mut p = Project::empty();
        // La piste 3 compresse sur la clé de la piste 5 : 5 passe avant 3.
        p.tracks[3].inserts[0] = keyed("compressor", 5);
        let order = p.routing_order().unwrap();
        let pos = |t| order.iter().position(|&x| x == t).unwrap();
        assert!(pos(5) < pos(3));
        // La piste 5 sort dans la 3 : passe toujours (5 avant 3).
        p.tracks[5].output = 3;
        assert!(p.routing_order().is_some());
        // Mais si la 3 sort dans la 5, la clé forme une boucle.
        p.tracks[5].output = MASTER;
        p.tracks[3].output = 5;
        assert!(p.routing_order().is_none());
        p.tracks[3].output = MASTER;
        // Boucle par deux clés croisées, par sa propre clé, par le master ou hors limites.
        p.tracks[5].inserts[2] = keyed("gate", 3);
        assert!(p.routing_order().is_none());
        p.tracks[5].inserts[2] = keyed("gate", 5);
        assert!(p.routing_order().is_none());
        p.tracks[5].inserts[2] = keyed("gate", MASTER);
        assert!(p.routing_order().is_none());
        p.tracks[5].inserts[2] = keyed("gate", p.track_count());
        assert!(p.routing_order().is_none());
        // Le master peut écouter n'importe quelle piste.
        p.tracks[5].inserts[2] = None;
        p.tracks[MASTER].inserts[0] = keyed("ducker", 3);
        assert!(p.routing_order().is_some());
    }

    #[test]
    fn sidechain_reaches_the_engine() {
        let mut p = Project::empty();
        p.tracks[3].inserts[1] = keyed("ducker", 4);
        let (mut engine, _) = p.build_engine(48_000.0, &no_samples);
        let mixer = engine.mixer_mut();
        assert_eq!(mixer.tracks[3].sidechain[1], Some(4));
        assert_eq!(mixer.tracks[3].inserts[1].as_ref().map(|fx| fx.kind()), Some("ducker"));
        let pos = |t| mixer.order.iter().position(|&x| x == t).unwrap();
        assert!(pos(4) < pos(3));
    }

    #[test]
    fn muted_takes_stay_in_the_project_but_do_not_play() {
        let mut p = Project::empty();
        p.audio_clips.push(audio("a.wav", 0, 96));
        p.audio_clips.push(AudioClipDef { muted: true, take: 1, ..audio("b.wav", 0, 96) });
        let loader = |_: &str| Some(Arc::new(Sample { sample_rate: 48_000.0, left: vec![0.0; 4], right: vec![0.0; 4], ..Default::default() }));
        assert_eq!(p.sequence_with(&loader).audio_clips.len(), 1);
    }

    fn audio(path: &str, start: u32, len: u32) -> AudioClipDef {
        AudioClipDef {
            path: path.into(),
            lane: 0,
            track: 2,
            start,
            len,
            offset: 0.0,
            gain: 1.0,
            fade_in: 0.0,
            fade_out: 0.0,
            take: 0,
            muted: false,
            ..Default::default()
        }
    }

    #[test]
    fn audio_clips_extend_the_song_and_need_their_sample() {
        let mut p = Project::empty();
        p.clips.push(ClipDef { pattern: 0, lane: 0, start: 0, len: TICKS_PER_BAR, offset: 0, muted: false });
        p.audio_clips.push(audio("a.wav", TICKS_PER_BAR * 4, 10));
        p.audio_clips.push(audio("absent.wav", 0, 10));
        assert_eq!(p.song_len(), TICKS_PER_BAR * 5);
        assert!(p.sequence().audio_clips.is_empty());
        let sample =
            Arc::new(Sample { name: "a".into(), sample_rate: 48_000.0, left: vec![0.5; 4], right: vec![0.5; 4], markers: Vec::new() });
        let loader = |path: &str| (path == "a.wav").then(|| sample.clone());
        let seq = p.sequence_with(&loader);
        assert_eq!(seq.audio_clips.len(), 1);
        assert_eq!(seq.audio_clips[0].start, TICKS_PER_BAR * 4);
        assert!(Arc::ptr_eq(&seq.audio_clips[0].sample, &sample));
        assert_eq!(seq.song_len, TICKS_PER_BAR * 5);
    }

    #[test]
    fn automation_points_stay_sorted_and_unique() {
        let mut p = Project::empty();
        let (t, s) = (AutoTarget::TrackGain(1), AutoScope::Song);
        assert!(p.automation_lane(t, s).is_none());
        assert_eq!(p.add_automation_point(t, s, 96, 0.5), 0);
        assert_eq!(p.add_automation_point(t, s, 0, 1.0), 0);
        assert_eq!(p.add_automation_point(t, s, 192, 0.0), 2);
        p.automation_lane_mut(t, s).unwrap().points[1].curve = 0.4;
        // Même tick : la valeur change, la tension reste.
        assert_eq!(p.add_automation_point(t, s, 96, 0.7), 1);
        let lane = p.automation_lane(t, s).unwrap();
        assert_eq!(
            lane.points.iter().map(|p| (p.tick, p.value)).collect::<Vec<_>>(),
            [(0, 1.0), (96, 0.7), (192, 0.0)]
        );
        assert_eq!(lane.points[1].curve, 0.4);
        assert_eq!(lane.value_at(500.0), Some(0.0));
        // Autre portée : autre courbe.
        p.add_automation_point(t, AutoScope::Pattern(0), 0, 0.3);
        assert_eq!(p.automation.len(), 2);
        assert_eq!(p.sequence().automation.len(), 2);

        assert_eq!(p.remove_automation_point(t, s, 50), None);
        assert_eq!(p.remove_automation_point(t, s, 96).map(|p| p.value), Some(0.7));
        p.remove_automation_point(t, s, 0);
        p.remove_automation_point(t, s, 192);
        assert!(p.automation_lane(t, s).is_none(), "courbe vide retirée");
        assert_eq!(p.automation.len(), 1);
    }

    #[test]
    fn removing_a_channel_updates_automation() {
        let mut p = Project::empty();
        for name in ["A", "B", "C"] {
            p.add_channel(name, InstrumentDef::new("pulse"), 60);
        }
        let s = AutoScope::Song;
        p.add_automation_point(AutoTarget::ChannelGain(0), s, 0, 1.0);
        p.add_automation_point(AutoTarget::ChannelPan(1), s, 0, 1.0);
        p.add_automation_point(AutoTarget::InstrumentParam { channel: 2, id: 3 }, s, 0, 1.0);
        p.add_automation_point(AutoTarget::TrackGain(1), s, 0, 1.0);
        p.remove_channel(1);
        let targets: Vec<AutoTarget> = p.automation.iter().map(|a| a.target).collect();
        assert_eq!(
            targets,
            [AutoTarget::ChannelGain(0), AutoTarget::InstrumentParam { channel: 1, id: 3 }, AutoTarget::TrackGain(1)]
        );
    }

    #[test]
    fn group_into_a_bus_routes_and_nests() {
        let mut p = Project::empty();
        let a = p.add_channel("Kick", InstrumentDef::new("drumsynth"), 60);
        let b = p.add_channel("Snare", InstrumentDef::new("drumsynth"), 60);
        let (ta, tb) = (p.channels[a].track, p.channels[b].track);
        let bus = p.add_bus(&[ta, tb, MASTER]).unwrap();
        assert_eq!(bus, DEFAULT_TRACKS, "a new bus is a new track at the end");
        assert_eq!((p.tracks[bus].kind, p.tracks[bus].name.as_str()), (TrackKind::Bus, "Bus 1"));
        assert_ne!(p.tracks[bus].color, 0);
        assert_eq!(p.bus_members(bus), [ta, tb]);
        assert_eq!(p.tracks[MASTER].output, MASTER);
        // Un bus dans un bus : le nouveau prend la place de ses pistes.
        let inner = p.add_bus(&[ta]).unwrap();
        assert_eq!(p.tracks[inner].name, "Bus 2");
        assert_eq!((p.tracks[ta].output, p.tracks[inner].output), (inner, bus));
        // Une boucle est refusée.
        assert!(!p.route_into(bus, inner));
        assert_eq!(p.tracks[bus].output, MASTER);
        // Le canal suivant ne tombe pas sur un bus.
        let c = p.add_channel("Hat", InstrumentDef::new("drumsynth"), 60);
        assert!(p.tracks[p.channels[c].track].kind == TrackKind::Normal);
        // Défaire le bus intérieur rend sa piste à son bus parent.
        p.dissolve(inner);
        assert_eq!(p.tracks[ta].output, bus);
        assert_eq!(p.tracks[inner], TrackDef::new(format!("Track {inner}")));
        assert!(!p.track_in_use(inner));
    }

    #[test]
    fn return_bus_has_a_wet_effect_and_a_send() {
        let mut p = Project::empty();
        let ch = p.add_channel("Voix", InstrumentDef::new("pulse"), 60);
        let t = p.channels[ch].track;
        let rev = p.add_return("reverb", Some(t)).unwrap();
        let fx = p.tracks[rev].inserts[0].as_ref().unwrap();
        assert_eq!((fx.kind.as_str(), fx.get(5)), ("reverb", Some(1.0)));
        assert_eq!(p.tracks[rev].name, "Reverb");
        assert_eq!(p.tracks[t].sends[0], (rev, 0.3));
        let second = p.add_return("reverb", None).unwrap();
        assert_eq!(p.tracks[second].name, "Reverb 1");
        let delay = p.add_return("delay", None).unwrap();
        assert_eq!(p.tracks[delay].inserts[0].as_ref().unwrap().kind, "delay");
        assert!(p.add_return("inconnu", None).is_none());
        assert!(p.routing_order().is_some());
        // Défaire le retour libère l'envoi.
        p.dissolve(rev);
        assert_eq!(p.tracks[t].sends[0], (MASTER, 0.0));
    }

    #[test]
    fn tracks_run_out_gracefully() {
        let mut p = Project::empty();
        let made = (0..ponzik_engine::mixer::MAX_TRACKS).filter_map(|_| p.add_bus(&[])).count();
        assert_eq!(made, ponzik_engine::mixer::MAX_TRACKS - DEFAULT_TRACKS);
        assert!(p.add_vca(&[]).is_none());
        assert!(p.free_or_new_track().is_some(), "the 32 first tracks are still free");
    }

    #[test]
    fn vca_reaches_the_engine() {
        let mut p = Project::empty();
        let vca = p.add_vca(&[2, 3, MASTER]).unwrap();
        assert_eq!(p.tracks[vca].kind, TrackKind::Vca);
        assert_eq!(p.vca_members(vca), [2, 3]);
        assert!(p.add_vca(&[vca]).is_some_and(|v| p.tracks[vca].vca.is_none() && v != vca), "un VCA ne pilote pas un VCA");
        let (mut engine, _) = p.build_engine(48_000.0, &no_samples);
        assert_eq!(engine.mixer_mut().tracks[2].vca, Some(vca));
        p.dissolve(vca);
        assert!(p.vca_members(vca).is_empty());
    }

    /// Crête du master pendant une mesure de deux canaux groupés dans un bus.
    fn bus_peak(edit: impl Fn(&mut Project, usize)) -> f32 {
        let mut p = Project::empty();
        for name in ["A", "B"] {
            let ch = p.add_channel(name, InstrumentDef::new("pulse"), 60);
            for step in [0, 4, 8, 12] {
                p.set_step(0, ch, step, true);
            }
        }
        let members: Vec<usize> = p.channels.iter().map(|c| c.track).collect();
        let bus = p.add_bus(&members).unwrap();
        // Un effet sur le bus, avec la clé d'entrée latérale sur une autre piste et de la latence.
        let mut comp = EffectDef::new("compressor");
        comp.sidechain = Some(5);
        p.tracks[bus].inserts[0] = Some(comp);
        p.tracks[bus].inserts[1] = Some(EffectDef::new("limiter"));
        edit(&mut p, bus);
        let (mut engine, mut handle) = p.build_engine(48_000.0, &no_samples);
        {
            let mixer = engine.mixer_mut();
            let pos = |t| mixer.order.iter().position(|&x| x == t).unwrap();
            assert!(members.iter().all(|&m| pos(m) < pos(bus)));
            assert!(pos(5) < pos(bus), "la clé passe avant le bus");
        }
        handle.send(Command::Play).unwrap();
        let mut buf = vec![0.0f32; 2 * 48_000];
        for chunk in buf.chunks_mut(1_024) {
            engine.process(chunk, 2);
        }
        assert!(engine.mixer_mut().latency() > 0, "la latence du limiteur du bus est compensée");
        buf.iter().fold(0.0f32, |m, s| m.max(s.abs()))
    }

    #[test]
    fn two_tracks_sum_through_a_bus_with_effects() {
        let open = bus_peak(|_, _| {});
        assert!(open > 0.05, "{open}");
        let muted = bus_peak(|p, bus| p.tracks[bus].muted = true);
        assert_eq!(muted, 0.0, "tout passe par le bus");
        let quiet = bus_peak(|p, bus| p.tracks[bus].gain = 0.25);
        assert!(quiet < open * 0.6, "{quiet} / {open}");
    }

    #[test]
    fn tempo_lane_becomes_the_tempo_map() {
        let mut p = Project::empty();
        p.bpm = 100.0;
        assert_eq!(p.tempo_map().bpm_at(5_000.0), 100.0);
        p.add_automation_point(AutoTarget::Tempo, AutoScope::Song, 384, 120.0);
        p.add_automation_point(AutoTarget::Tempo, AutoScope::Song, 768, 240.0);
        let map = p.tempo_map();
        // Held before the first point, straight segments become exact ramps.
        assert_eq!(map.bpm_at(0.0), 120.0);
        assert!((map.bpm_at(576.0) - 180.0).abs() < 1e-9);
        assert_eq!(map.bpm_at(2_000.0), 240.0);
        // A curved segment: a chain of ramps through the curve.
        p.automation[0].points[0].curve = 0.6;
        let map = p.tempo_map();
        let lane = p.automation_lane(AutoTarget::Tempo, AutoScope::Song).unwrap();
        for tick in [400.0, 500.0, 650.0] {
            let want = lane.value_at(tick).unwrap() as f64;
            assert!((map.bpm_at(tick) - want).abs() < 3.0, "{tick}: {} vs {want}", map.bpm_at(tick));
        }
        assert_eq!(p.sequence().tempo, map);
    }
}
