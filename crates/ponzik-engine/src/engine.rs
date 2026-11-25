use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::time::Instant;

use ponzik_dsp::util::balance_gains;

use crate::audition::Audition;
use crate::effect::Effect;
use crate::freeze::{FreezeClock, Frozen, Tap};
use crate::voice::{VoiceInput, VoiceReport, VoiceReports, VoiceSettings};
use crate::input::{CALIBRATION_CLICKS, InputChain, InputConsumer, InputSource, InputStrip, RecordBuffer, RecordChunk, calibration_click, deinterleave};
use crate::instrument::Instrument;
use crate::meter::{PeakMeter, peak};
use crate::midi_out::{MidiClock, MidiLink, MidiOut};
use crate::effects::fx_rack::{MacroMap, RackLayout};
use crate::mixer::{INSERT_SLOTS, MASTER, MAX_TRACKS, Mixer, SEND_SLOTS, TrackRemap};
use crate::modulation::{ModClock, ModInput, ModSet};
use crate::note_fx::NoteFx;
use crate::pool::{Parallelism, WorkerPool};
use crate::rack::{Channel, NOTE_FX_SLOTS};
use crate::ring::{self, Consumer, Producer};
use crate::sequence::{AudioClip, AutoTarget, PPQ, PlayMode, SeqEvent, Sequence, equal_power, fade_shape};
use crate::tempo_map::{TempoMap, TempoView};

/// Taille max d'un bloc de traitement (l'automation est évaluée à chaque bloc).
const MAX_BLOCK: usize = 256;
/// Clips audio suivis d'un bloc à l'autre ; au-delà, la position est recalculée
/// depuis le tick à chaque bloc.
const MAX_AUDIO_VOICES: usize = 4_096;
/// Fondu minimal des clips audio, en secondes.
const MIN_FADE: f32 = 0.002;
const COMMAND_QUEUE: usize = 4_096;
/// Pistes enregistrées en même temps, au plus.
pub const MAX_RECORDINGS: usize = 8;
const GARBAGE_QUEUE: usize = 1_024;
const MAX_EVENTS: usize = 8_192;
pub const MAX_CHANNELS: usize = 128;
/// Échantillons du master gardés pour l'oscilloscope (≈ 0,7 s à 24 kHz).
const SCOPE_QUEUE: usize = 16_384;
const SCOPE_DECIMATION: u32 = 2;
/// Plus grande latence compensée par le mixer, en secondes.
const MAX_LATENCY_SECONDS: f32 = 1.0;
/// Constante de temps du lissage des mesures de charge par piste et par appareil.
const CPU_SMOOTHING_SECONDS: f32 = 0.3;
/// Sous-bloc le plus court dont les canaux sont répartis entre les threads.
const MIN_PARALLEL_FRAMES: usize = 32;
/// Automation lanes that can be held at once (see [`Command::HoldAutomation`]).
pub const MAX_HELD: usize = 32;

/// Ordres de l'interface au moteur. Les objets lourds (instruments, effets,
/// séquences) sont préparés par l'interface ; ceux qu'ils remplacent repartent
/// par la file [`Garbage`] pour être libérés hors du thread audio.
pub enum Command {
    Play,
    /// Pré-compte : `ticks` de clics de métronome (même coupé), puis lecture
    /// depuis la position courante. La séquence et l'enregistrement attendent la
    /// fin du pré-compte.
    CountIn(u32),
    /// Premier appui : pause ; second appui : retour au début.
    Stop,
    /// Saute à une position, en ticks.
    Locate(f64),
    /// Sets the first tempo of the tempo map (the whole tempo when the song
    /// has no tempo change).
    SetBpm(f32),
    SetSequence(Box<Sequence>),
    AddChannel(Box<Channel>),
    RemoveChannel(usize),
    ReplaceInstrument { channel: usize, instrument: Box<dyn Instrument> },
    /// Replaces a whole channel (instrument, note effects, modulators,
    /// freeze); the old one comes back as [`Garbage::Channel`].
    ReplaceChannel { channel: usize, ch: Box<Channel> },
    SetChannelGain { channel: usize, gain: f32 },
    SetChannelPan { channel: usize, pan: f32 },
    SetChannelMute { channel: usize, muted: bool },
    /// Portamento of a channel in seconds (0: off).
    SetChannelPorta { channel: usize, secs: f32 },
    SetChannelTrack { channel: usize, track: usize },
    SetInstrumentParam { channel: usize, id: u32, value: f32 },
    /// Place (ou retire) un effet de notes, déjà réglé à la fréquence
    /// d'échantillonnage ; le canal est coupé pour ne laisser aucune note pendue.
    SetNoteFx { channel: usize, slot: usize, fx: Option<Box<dyn NoteFx>> },
    SetNoteFxParam { channel: usize, slot: usize, id: u32, value: f32 },
    /// Note jouée en direct ; passe par les effets de notes du canal.
    NoteOn { channel: usize, note: u8, velocity: f32 },
    NoteOff { channel: usize, note: u8 },
    SetTrackGain { track: usize, gain: f32 },
    SetTrackPan { track: usize, pan: f32 },
    SetTrackMute { track: usize, muted: bool },
    SetTrackOutput { track: usize, output: usize },
    /// Piste VCA qui pilote le volume de `track`, ou aucune.
    SetTrackVca { track: usize, vca: Option<usize> },
    SetTrackSend { track: usize, slot: usize, target: usize, amount: f32 },
    SetRoutingOrder(Box<[usize]>),
    /// New track layout (tracks added, deleted or moved): the engine moves
    /// the state of the kept tracks into the new mixer and renumbers what
    /// points at tracks (routing, channels, monitoring, recordings); the old
    /// mixer comes back as [`Garbage::Mixer`]. Built by [`track_remap`].
    RemapTracks(Box<TrackRemap>),
    SetInsert { track: usize, slot: usize, effect: Option<Box<dyn Effect>> },
    SetInsertBypass { track: usize, slot: usize, bypass: bool },
    /// Branche la piste clé d'un insert (`None` : débranche). L'interface doit
    /// aussi envoyer le nouvel ordre de routage ([`Command::SetRoutingOrder`]) ;
    /// une clé pas encore traitée dans le bloc est lue comme du silence.
    SetInsertSidechain { track: usize, slot: usize, key: Option<usize> },
    SetEffectParam { track: usize, slot: usize, id: u32, value: f32 },
    /// Order and bypass of the devices of the rack in an insert slot.
    SetRackLayout { track: usize, slot: usize, layout: RackLayout },
    /// Places (or removes) a device of a rack, already at the sample rate; the
    /// old one comes back as [`Garbage::Effect`].
    SetRackUnit { track: usize, slot: usize, unit: usize, effect: Option<Box<dyn Effect>> },
    /// Sets (or clears) one macro mapping of a rack.
    SetRackMapping { track: usize, slot: usize, index: usize, map: Option<MacroMap> },
    /// Places (or removes) a device of the Instrument Rack of a channel,
    /// already at the sample rate; the old one comes back as garbage.
    SetLayerDevice { channel: usize, unit: usize, device: crate::instruments::inst_rack::LayerDevice },
    /// Zones, switches and bypasses of one chain of a channel's Instrument Rack.
    SetLayerZone { channel: usize, chain: usize, zone: crate::instruments::inst_rack::ChainZone },
    /// Sets (or clears) one macro mapping of a channel's Instrument Rack.
    SetLayerMapping { channel: usize, index: usize, map: Option<MacroMap> },
    /// Modulators of a track (targets: its inserts); the old set comes back as
    /// [`Garbage::Modulators`], its running state carried over.
    SetTrackModulators { track: usize, mods: Option<Box<ModSet>> },
    /// Modulators of a channel (targets: its instrument).
    SetChannelModulators { channel: usize, mods: Option<Box<ModSet>> },
    /// Freezes a channel (its render replaces its instrument) or thaws it
    /// (`None`); the old player comes back as [`Garbage::Frozen`].
    SetChannelFrozen { channel: usize, frozen: Option<Box<Frozen>> },
    /// Freezes a mixer track (its render replaces its input and inserts) or
    /// thaws it; the old player comes back as [`Garbage::Frozen`].
    SetTrackFrozen { track: usize, frozen: Option<Box<Frozen>> },
    SetMetronome(bool),
    /// While a lane is being recorded live, its playback stops overriding the
    /// parameter (the hand wins); `hold: false` gives it back to the lane.
    HoldAutomation { target: AutoTarget, hold: bool },
    /// Branche (ou débranche) l'entrée audio ; l'ancienne repart en [`Garbage::Input`].
    SetInput(Option<Box<InputConsumer>>),
    /// Écoute de l'entrée en direct sur une piste (`None` : coupée), ajoutée à
    /// son entrée avant ses inserts.
    SetMonitor { track: usize, source: Option<InputSource> },
    /// Écoute à faible latence : sur le trajet des pistes écoutées jusqu'au
    /// master, les inserts qui anticipent (latence non nulle) sont court-circuités
    /// et la compensation de latence ne retarde plus le signal écouté.
    SetLowLatencyMonitoring(bool),
    /// Smart disable of the inserts (see [`Mixer::set_smart_disable`]).
    SetSmartDisable(bool),
    /// Tranche d'entrée : gain, coupe-bas, porte.
    SetInputStrip(InputStrip),
    /// Arme l'enregistrement de l'entrée sur `track` dans `buffer`, préalloué
    /// par l'interface (sa source dit quels canaux garder) ; il n'est rempli que
    /// transport lancé. Désarmer, réarmer la piste ou arrêter le transport rend
    /// le tampon via [`Garbage::Recording`]. Jusqu'à [`MAX_RECORDINGS`] pistes à
    /// la fois ; `armed` sans tampon désarme la piste.
    Record { track: usize, armed: bool, buffer: Option<Box<RecordBuffer>> },
    /// Allonge la prise en cours de `track` : elle continue dans `chunk` quand
    /// son tampon est plein (voir [`EngineHandle::record_room`]). Sans prise
    /// en cours (ou morceau d'un autre format), il revient en [`Garbage::RecordChunk`].
    RecordChunk { track: usize, chunk: Box<RecordChunk> },
    /// Pré-écoute de l'Atelier, ajoutée à la sortie hors du mixer : remplace la
    /// voix en cours (rendue en [`Garbage::Audition`]) ; `None` l'arrête en fondu.
    Audition(Option<Box<Audition>>),
    /// Gain of the audition voice playing (the browser's preview volume).
    AuditionGain(f32),
    /// Calibrage de la latence : le moteur joue des clics
    /// ([`crate::input::CALIBRATION_CLICKS`]) directement sur la sortie et
    /// enregistre l'entrée brute dans `buffer`, rendu plein via
    /// [`Garbage::Calibration`]. `None` interrompt un calibrage en cours.
    Calibrate(Option<Box<RecordBuffer>>),
    /// Free recording for the audio editor: the input (after the input strip)
    /// is written into `buffer` whether the transport runs or not, no click,
    /// no track. It comes back full, replaced or stopped (`None`) as
    /// [`Garbage::Capture`].
    Capture(Option<Box<RecordBuffer>>),
    /// Branche (ou débranche) la sortie MIDI : notes des instruments externes
    /// et horloge. L'ancienne repart en [`Garbage::MidiOut`].
    SetMidiOut(Option<Box<MidiOut>>),
    /// Ports (bit par indice de port) qui reçoivent l'horloge MIDI, Start,
    /// Stop, Continue et Song Position Pointer ; 0 : horloge coupée.
    SetMidiClock { ports: u32 },
    /// Voice to notes: the input plays the chosen channel's instrument.
    SetVoice(VoiceSettings),
}

impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Command::Play => "Play",
            Command::CountIn(_) => "CountIn",
            Command::Stop => "Stop",
            Command::Locate(_) => "Locate",
            Command::SetBpm(_) => "SetBpm",
            Command::SetSequence(_) => "SetSequence",
            Command::AddChannel(_) => "AddChannel",
            Command::RemoveChannel(_) => "RemoveChannel",
            Command::ReplaceInstrument { .. } => "ReplaceInstrument",
            Command::ReplaceChannel { .. } => "ReplaceChannel",
            Command::SetChannelGain { .. } => "SetChannelGain",
            Command::SetChannelPan { .. } => "SetChannelPan",
            Command::SetChannelMute { .. } => "SetChannelMute",
            Command::SetChannelPorta { .. } => "SetChannelPorta",
            Command::SetChannelTrack { .. } => "SetChannelTrack",
            Command::SetInstrumentParam { .. } => "SetInstrumentParam",
            Command::SetNoteFx { .. } => "SetNoteFx",
            Command::SetNoteFxParam { .. } => "SetNoteFxParam",
            Command::NoteOn { .. } => "NoteOn",
            Command::NoteOff { .. } => "NoteOff",
            Command::SetTrackGain { .. } => "SetTrackGain",
            Command::SetTrackPan { .. } => "SetTrackPan",
            Command::SetTrackMute { .. } => "SetTrackMute",
            Command::SetTrackOutput { .. } => "SetTrackOutput",
            Command::SetTrackVca { .. } => "SetTrackVca",
            Command::SetTrackSend { .. } => "SetTrackSend",
            Command::SetRoutingOrder(_) => "SetRoutingOrder",
            Command::RemapTracks(_) => "RemapTracks",
            Command::SetInsert { .. } => "SetInsert",
            Command::SetInsertBypass { .. } => "SetInsertBypass",
            Command::SetInsertSidechain { .. } => "SetInsertSidechain",
            Command::SetEffectParam { .. } => "SetEffectParam",
            Command::SetRackLayout { .. } => "SetRackLayout",
            Command::SetRackUnit { .. } => "SetRackUnit",
            Command::SetRackMapping { .. } => "SetRackMapping",
            Command::SetLayerDevice { .. } => "SetLayerDevice",
            Command::SetLayerZone { .. } => "SetLayerZone",
            Command::SetLayerMapping { .. } => "SetLayerMapping",
            Command::SetTrackModulators { .. } => "SetTrackModulators",
            Command::SetChannelModulators { .. } => "SetChannelModulators",
            Command::SetChannelFrozen { .. } => "SetChannelFrozen",
            Command::SetTrackFrozen { .. } => "SetTrackFrozen",
            Command::SetMetronome(_) => "SetMetronome",
            Command::HoldAutomation { .. } => "HoldAutomation",
            Command::SetInput(_) => "SetInput",
            Command::SetMonitor { .. } => "SetMonitor",
            Command::SetLowLatencyMonitoring(_) => "SetLowLatencyMonitoring",
            Command::SetSmartDisable(_) => "SetSmartDisable",
            Command::SetInputStrip(_) => "SetInputStrip",
            Command::Record { .. } => "Record",
            Command::RecordChunk { .. } => "RecordChunk",
            Command::Audition(_) => "Audition",
            Command::AuditionGain(_) => "AuditionGain",
            Command::Calibrate(_) => "Calibrate",
            Command::Capture(_) => "Capture",
            Command::SetMidiOut(_) => "SetMidiOut",
            Command::SetMidiClock { .. } => "SetMidiClock",
            Command::SetVoice(_) => "SetVoice",
        };
        f.write_str(name)
    }
}

/// Objets remplacés, rendus à l'interface pour être libérés.
pub enum Garbage {
    Sequence(Box<Sequence>),
    Channel(Box<Channel>),
    Instrument(Box<dyn Instrument>),
    Effect(Box<dyn Effect>),
    NoteFx(Box<dyn NoteFx>),
    Modulators(Box<ModSet>),
    Routing(Box<[usize]>),
    Mixer(Box<TrackRemap>),
    Input(Box<InputConsumer>),
    /// Enregistrement terminé : gardé à part par [`EngineHandle::take_recordings`].
    Recording(Box<RecordBuffer>),
    Audition(Box<Audition>),
    /// Calibrage terminé : gardé à part par [`EngineHandle::take_calibration`].
    Calibration(Box<RecordBuffer>),
    /// Free recording of the audio editor: kept apart by [`EngineHandle::take_capture`].
    Capture(Box<RecordBuffer>),
    MidiOut(Box<MidiOut>),
    RecordChunk(Box<RecordChunk>),
    Frozen(Box<Frozen>),
}

/// Ce que le moteur publie pour l'interface.
struct SharedState {
    playing: AtomicBool,
    /// Position en ticks (bits d'un `f64`).
    position: AtomicU64,
    /// Pré-compte restant en ticks (bits d'un `f64`), 0 hors pré-compte.
    count_in: AtomicU64,
    channel_peaks: Vec<PeakMeter>,
    track_peaks: Vec<PeakMeter>,
    input_peak: PeakMeter,
    /// L'entrée a saturé depuis la dernière lecture.
    input_clip: AtomicBool,
    /// Gain de la porte d'entrée (bits d'un `f32`, 1 : ouverte).
    input_gate: AtomicU32,
    /// Pire charge DSP depuis la dernière lecture : temps de calcul / durée du bloc.
    load: AtomicU32,
    /// Latence totale au master, en échantillons.
    latency: AtomicU32,
    /// Position de la pré-écoute en images de l'échantillon (bits d'un `f64`, −1 : rien).
    audition: AtomicU64,
    /// Charge lissée (bits d'un `f32`, part du temps réel) de chaque piste,
    /// de chaque insert (`piste × INSERT_SLOTS + emplacement`) et de chaque canal.
    track_cpu: Vec<AtomicU32>,
    insert_cpu: Vec<AtomicU32>,
    channel_cpu: Vec<AtomicU32>,
    /// Sleeping inserts of each track (bit `slot`), see [`Engine::set_smart_disable`].
    insert_sleep: Vec<AtomicU8>,
    /// Threads de calcul en plus du thread audio.
    workers: AtomicU32,
    /// Les threads de calcul servent en ce moment (choix adaptatif).
    parallel: AtomicBool,
    /// Place libre de chaque prise en cours : `(piste + 1) << 32 | trames`
    /// (0 : emplacement libre).
    record_room: [AtomicU64; MAX_RECORDINGS],
}

/// Lit une charge publiée (bits d'un `f32`).
fn load_of(cell: Option<&AtomicU32>) -> f32 {
    cell.map_or(0.0, |c| f32::from_bits(c.load(Ordering::Relaxed)))
}

/// Pointeur partagé avec les threads de calcul ; chaque tâche n'en touche
/// qu'une partie distincte.
struct SyncPtr<T>(*mut T);

impl<T> Clone for SyncPtr<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for SyncPtr<T> {}

// SAFETY: les tâches d'un même travail accèdent à des éléments distincts.
unsafe impl<T> Send for SyncPtr<T> {}
unsafe impl<T> Sync for SyncPtr<T> {}

impl<T> SyncPtr<T> {
    /// Élément `i` (méthode : la fermeture capture le pointeur entier, partageable).
    fn at(self, i: usize) -> *mut T {
        self.0.wrapping_add(i)
    }
}

/// Côté interface : envoie des commandes, lit l'état publié.
pub struct EngineHandle {
    commands: Producer<Command>,
    state: Arc<SharedState>,
    scope: Consumer<f32>,
    garbage: Consumer<Garbage>,
    /// Les boîtes envoyées par l'interface lui reviennent telles quelles.
    #[allow(clippy::vec_box)]
    recordings: Vec<Box<RecordBuffer>>,
    calibration: Option<Box<RecordBuffer>>,
    capture: Option<Box<RecordBuffer>>,
    sample_rate: f32,
    voice: VoiceReports,
}

impl EngineHandle {
    /// Rend la commande si la file est pleine.
    pub fn send(&mut self, command: Command) -> Result<(), Command> {
        self.collect_garbage();
        self.commands.push(command)
    }

    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    pub fn is_playing(&self) -> bool {
        self.state.playing.load(Ordering::Relaxed)
    }

    /// Position de lecture en ticks.
    pub fn position(&self) -> f64 {
        f64::from_bits(self.state.position.load(Ordering::Relaxed))
    }

    /// Pré-compte restant en ticks (0 : pas de pré-compte en cours).
    pub fn count_in(&self) -> f64 {
        f64::from_bits(self.state.count_in.load(Ordering::Relaxed))
    }

    /// Crête [gauche, droite] du canal depuis le dernier appel (après gain et pan).
    pub fn take_channel_peak(&self, channel: usize) -> [f32; 2] {
        self.state.channel_peaks.get(channel).map_or([0.0; 2], PeakMeter::take)
    }

    /// Crête [gauche, droite] d'une piste de mixer depuis le dernier appel.
    pub fn take_track_peak(&self, track: usize) -> [f32; 2] {
        self.state.track_peaks.get(track).map_or([0.0; 2], PeakMeter::take)
    }

    pub fn take_master_peak(&self) -> [f32; 2] {
        self.take_track_peak(MASTER)
    }

    /// Crête [gauche, droite] de l'entrée audio depuis le dernier appel.
    pub fn take_input_peak(&self) -> [f32; 2] {
        self.state.input_peak.take()
    }

    /// Vrai si l'entrée a saturé depuis le dernier appel (convertisseur à fond ou
    /// gain d'entrée trop fort).
    pub fn take_input_clip(&self) -> bool {
        self.state.input_clip.swap(false, Ordering::Relaxed)
    }

    /// Gain courant de la porte d'entrée (1 : ouverte ou désactivée).
    pub fn input_gate(&self) -> f32 {
        f32::from_bits(self.state.input_gate.load(Ordering::Relaxed))
    }

    /// Pire charge DSP (1.0 = le bloc a pris tout son temps réel) depuis le dernier appel.
    pub fn take_load(&self) -> f32 {
        f32::from_bits(self.state.load.swap(0, Ordering::Relaxed))
    }

    /// Latence totale au master, en échantillons : retard du chemin le plus lent
    /// (inserts des pistes et du master), compensé sur tous les autres.
    pub fn latency(&self) -> usize {
        self.state.latency.load(Ordering::Relaxed) as usize
    }

    /// Position de la pré-écoute en images de l'échantillon, si elle joue.
    pub fn audition_position(&self) -> Option<f64> {
        Some(f64::from_bits(self.state.audition.load(Ordering::Relaxed))).filter(|p| *p >= 0.0)
    }

    /// Charge de calcul d'une piste de mixer (inserts et fader), en part du
    /// temps réel (1.0 : un cœur entier), lissée sur ≈ 0,3 s.
    pub fn track_cpu(&self, track: usize) -> f32 {
        load_of(self.state.track_cpu.get(track))
    }

    /// Charge de calcul d'un insert, en part du temps réel, lissée.
    pub fn insert_cpu(&self, track: usize, slot: usize) -> f32 {
        if slot >= INSERT_SLOTS {
            return 0.0;
        }
        load_of(self.state.insert_cpu.get(track * INSERT_SLOTS + slot))
    }

    /// True if the insert sleeps: smart disable on, silence in and out.
    pub fn insert_asleep(&self, track: usize, slot: usize) -> bool {
        slot < INSERT_SLOTS && self.state.insert_sleep.get(track).is_some_and(|m| m.load(Ordering::Relaxed) & (1 << slot) != 0)
    }

    /// Charge de calcul de l'instrument d'un canal, en part du temps réel, lissée.
    pub fn channel_cpu(&self, channel: usize) -> f32 {
        load_of(self.state.channel_cpu.get(channel))
    }

    /// Threads de calcul du moteur en plus du thread audio (0 : un seul cœur).
    pub fn worker_threads(&self) -> usize {
        self.state.workers.load(Ordering::Relaxed) as usize
    }

    /// Vrai si les threads de calcul partagent le travail en ce moment ; faux
    /// s'ils dorment parce que le thread audio suffit (voir [`Engine::set_adaptive_threads`]).
    pub fn parallel_active(&self) -> bool {
        self.state.parallel.load(Ordering::Relaxed)
    }

    /// Latence totale au master, en secondes.
    pub fn latency_seconds(&self) -> f32 {
        self.latency() as f32 / self.sample_rate.max(1.0)
    }

    /// Échantillons mono du master (24 kHz) arrivés depuis le dernier appel.
    pub fn drain_scope(&mut self, mut f: impl FnMut(f32)) {
        while let Some(s) = self.scope.pop() {
            f(s);
        }
    }

    /// Libère les objets que le moteur a remplacés ; les enregistrements sont mis de côté.
    pub fn collect_garbage(&mut self) {
        while let Some(g) = self.garbage.pop() {
            match g {
                Garbage::Recording(rec) => self.recordings.push(rec),
                Garbage::Calibration(rec) => self.calibration = Some(rec),
                Garbage::Capture(rec) => self.capture = Some(rec),
                _ => {}
            }
        }
    }

    /// Next object the engine gave back, to inspect it rather than drop it
    /// ([`EngineHandle::collect_garbage`] drops them all).
    pub fn take_garbage(&mut self) -> Option<Garbage> {
        self.garbage.pop()
    }

    /// Free recording given back (full or stopped) since the last call.
    pub fn take_capture(&mut self) -> Option<Box<RecordBuffer>> {
        self.collect_garbage();
        self.capture.take()
    }

    /// Calibrage terminé (ou interrompu) depuis le dernier appel.
    pub fn take_calibration(&mut self) -> Option<Box<RecordBuffer>> {
        self.collect_garbage();
        self.calibration.take()
    }

    /// Trames encore libres dans la prise en cours de `track` (`None` : pas de
    /// prise) : l'interface envoie un [`Command::RecordChunk`] avant qu'elle soit pleine.
    pub fn record_room(&self, track: usize) -> Option<usize> {
        self.state.record_room.iter().find_map(|cell| {
            let v = cell.load(Ordering::Relaxed);
            ((v >> 32) as usize == track + 1).then_some((v & 0xffff_ffff) as usize)
        })
    }

    /// Enregistrements terminés depuis le dernier appel (tampons vides compris).
    /// Voice to notes: pitch frames and notes played since the last call.
    pub fn drain_voice(&mut self, f: impl FnMut(VoiceReport)) {
        self.voice.drain(f);
    }

    pub fn take_recordings(&mut self) -> Vec<Box<RecordBuffer>> {
        self.collect_garbage();
        std::mem::take(&mut self.recordings)
    }
}

pub struct Engine {
    sample_rate: f32,
    channels: Vec<Channel>,
    mixer: Mixer,
    sequence: Box<Sequence>,
    commands: Consumer<Command>,
    state: Arc<SharedState>,
    scope: Producer<f32>,
    scope_phase: u32,
    garbage: Producer<Garbage>,

    /// Horloge monotone en échantillons, avance même à l'arrêt (queues de release).
    clock: u64,
    playing: bool,
    bpm: f64,
    /// Position de lecture en ticks.
    pos: f64,
    /// Position anchor: at `anchor_clock`, the tempo map was at `anchor_secs`
    /// seconds. Positions are recomputed from it (no rounding drift) and
    /// converted through the tempo map, so ramps stay sample-accurate.
    anchor_secs: f64,
    anchor_clock: u64,
    metronome: bool,
    /// Automation lanes not played back (recorded live), at most [`MAX_HELD`].
    held: [Option<AutoTarget>; MAX_HELD],
    /// Pré-compte : ticks restants et durée totale ; la lecture démarre à 0.
    count_in: f64,
    count_len: f64,
    click_phase: f32,
    click_env: f32,
    click_hz: f32,

    events: Vec<SeqEvent>,
    /// Controller events of the block: (frame, automation lane, value).
    ctl_events: Vec<(usize, usize, f32)>,
    /// For each event of the block: its frame in the block and its length in samples.
    event_frames: Vec<(usize, u64)>,
    scratch_l: Vec<f32>,
    scratch_r: Vec<f32>,
    /// Lecture des clips audio, un par clip de la séquence.
    voices: Vec<AudioVoice>,
    /// Tick de fin du dernier bloc joué (NaN à l'arrêt) : un bloc qui commence
    /// ailleurs (saut, boucle) relance les clips audio depuis leur position.
    audio_next: f64,

    input: Option<Box<InputConsumer>>,
    input_l: Vec<f32>,
    input_r: Vec<f32>,
    strip: InputChain,
    /// Écoute de l'entrée, piste par piste.
    low_latency: bool,
    /// Smart disable of the inserts, kept when the mixer is replaced.
    smart_disable: bool,
    /// Enregistrements armés, une piste chacun.
    recordings: [Option<Box<RecordBuffer>>; MAX_RECORDINGS],
    /// Calibrage en cours : entrée brute, et trames écoulées depuis son début.
    calibration: Option<Box<RecordBuffer>>,
    cal_clock: usize,
    /// Free recording of the audio editor.
    capture: Option<Box<RecordBuffer>>,
    /// Prises et calibrages que la file de retour pleine n'a pas pu prendre ;
    /// réessayés au bloc suivant (capacité réservée, jamais dépassée).
    returning: Vec<Garbage>,
    /// Pré-écoute de l'Atelier.
    audition: Option<Box<Audition>>,
    /// Sortie MIDI (instruments externes) et horloge MIDI.
    midi: MidiLink,
    midi_clock: MidiClock,
    /// Threads de calcul (aucun : tout sur le thread audio, chemin de référence).
    pool: Option<WorkerPool>,
    /// Choix adaptatif des threads de calcul (`None` : toujours utilisés).
    parallelism: Option<Parallelism>,
    /// Les threads de calcul servent aux prochains blocs.
    parallel_on: bool,
    /// Rendu de chaque canal sur le bloc, `MAX_BLOCK` images par canal, quand
    /// les canaux sont rendus en parallèle ; additionné ensuite dans l'ordre.
    lanes_l: Vec<f32>,
    lanes_r: Vec<f32>,
    /// Voice to notes: tracker of the input and its notes of the block.
    voice: VoiceInput,
}

/// État de lecture d'un clip audio entre deux blocs.
#[derive(Clone, Copy, Debug, Default)]
struct AudioVoice {
    active: bool,
    /// Position dans l'échantillon, en images de la source.
    src: f64,
    /// Échantillons joués depuis le (re)démarrage, pour le fondu d'entrée.
    run: f64,
}

/// Fastest tick rate (ticks per second, at the highest tempo): bounds how far
/// before its start a crossfaded clip can sound.
const MAX_TICKS_PER_SEC: f64 = crate::tempo_map::MAX_BPM / 60.0 * PPQ as f64;

/// A block of audio clip playback: where it starts in the song.
struct ClipBlock<'a> {
    view: &'a TempoView<'a>,
    /// Tick and time (seconds) of the first sample.
    pos_start: f64,
    t0: f64,
    n: usize,
    sr: f64,
    /// Follows the previous block: clips resume where they were.
    contiguous: bool,
}

/// Plays one audio clip over a block, sample-accurately, into the buffers
/// `out` gives (asked for only when the clip sounds in this block); returns
/// the voice to resume from on the next block. Unwarped clips keep their place
/// in ticks and play at their own speed (linear interpolation): they don't
/// follow tempo changes. Warped clips play their rendered stretch, or until it
/// is ready their file resampled along the tempo map; both are computed from
/// the time of each sample, so a clip is always in time, whatever the block
/// size. Crossfaded edges (comp segments) sound half a crossfade beyond the
/// clip, on an equal-power curve.
fn play_audio_clip<'o>(clip: &AudioClip, prev: AudioVoice, b: &ClipBlock, out: impl FnOnce() -> (&'o mut [f32], &'o mut [f32])) -> AudioVoice {
    let (view, t0, n, sr) = (b.view, b.t0, b.n, b.sr);
    let sample = &*clip.sample;
    let len = sample.len();
    let (start, end) = (clip.start as f64, clip.start as f64 + clip.len as f64);
    let (xin, xout) = (clip.xfade_in.max(0.0) as f64, clip.xfade_out.max(0.0) as f64);
    let margin = 0.5 * xin.max(xout) * MAX_TICKS_PER_SEC + 1.0;
    let margin = if xin > 0.0 || xout > 0.0 { margin } else { 0.0 };
    let to = view.tick_at_secs(t0 + n as f64 / sr);
    if len == 0 || end + margin <= b.pos_start || start - margin >= to {
        return AudioVoice::default();
    }
    // Output samples from the block start (k = 0) to the clip start and end,
    // then to the edges of the crossfade windows around them.
    let start_at = (view.secs_at(start) - t0) * sr;
    let end_at = (view.secs_at(end) - t0) * sr;
    let (xin, xout) = (xin * sr, xout * sr);
    let (win_start, win_end) = (start_at - 0.5 * xin, end_at + 0.5 * xout);
    let first = |at: f64| ((at - 1e-6).ceil().max(0.0) as usize).min(n);
    let (k0, k1) = (first(win_start), first(win_end));
    let src_rate = sample.sample_rate.max(1.0) as f64;
    let ratio = src_rate / sr;
    let resume = prev.active && b.contiguous && k0 == 0;
    let (mut p, mut run) = if resume {
        (prev.src, prev.run)
    } else {
        // The clip start plays the offset; a crossfade before it reads earlier
        // (nothing before the file start).
        let lead = if xin > 0.0 { k0 as f64 - start_at } else { (k0 as f64 - start_at).max(0.0) };
        let secs = clip.offset.max(0.0) + lead / sr;
        // Après un saut, petit fondu d'entrée ; sinon (nouvelle séquence,
        // début du clip) seul le fondu du clip compte.
        let p = secs * src_rate;
        // At the engine's rate, the clip starts on a whole frame of its file
        // (its first sample on the first output sample at or after its
        // start), copied without interpolation.
        let p = if (ratio - 1.0).abs() < 1e-12 { (p + 1e-6).floor() } else { p };
        (if clip.looped { p.rem_euclid(len as f64) } else { p }, if b.contiguous { f64::MAX / 2.0 } else { 0.0 })
    };
    let fade_in = clip.fade_in.max(MIN_FADE) as f64 * sr;
    let fade_out = clip.fade_out.max(MIN_FADE) as f64 * sr;
    let right = if sample.right.len() == len { &sample.right } else { &sample.left };
    let last = len - 1;
    let at = |ch: &[f32], i: usize| if clip.reverse { ch[last - i] } else { ch[i] };
    let rendered = clip.warp.as_ref().and_then(|w| w.rendered.as_deref()).filter(|r| r.sample_rate as f64 == sr && !r.is_empty());
    let warp_origin = clip.warp.as_ref().map_or(0.0, |w| w.map.beat_at(clip.offset.max(0.0)));
    // Session clip starting over every cycle: its length and phase in output
    // samples (the session tempo is constant).
    let frames_of = |ticks: u32| (view.secs_at(start + ticks as f64) - view.secs_at(start)) * sr;
    let (cycle, phase) = if clip.cycle > 0 { (frames_of(clip.cycle).max(1.0), frames_of(clip.phase % clip.cycle)) } else { (0.0, 0.0) };
    let (tl, tr) = if k0 < k1 { out() } else { (&mut [][..], &mut [][..]) };
    let mut ended = false;
    for k in k0..k1 {
        let from_start = k as f64 - start_at;
        let (since, until_cycle) = if cycle > 0.0 && from_start >= 0.0 {
            let local = (from_start + phase).rem_euclid(cycle);
            // Unwarped: the file from the offset, at its own speed.
            p = clip.offset.max(0.0) * src_rate + local * ratio;
            if clip.looped {
                p = p.rem_euclid(len as f64);
            }
            (local, cycle - local)
        } else {
            (from_start, f64::MAX)
        };
        let (l, r, until_src) = match (&clip.warp, rendered) {
            (Some(_), Some(buf)) => {
                let i = since.round();
                if i < 0.0 || (cycle > 0.0 && i as usize >= buf.len()) {
                    (0.0, 0.0, f64::MAX)
                } else if i as usize >= buf.len() {
                    ended = true;
                    break;
                } else {
                    let i = i as usize;
                    let rr = if buf.right.len() == buf.len() { &buf.right } else { &buf.left };
                    (buf.left[i], rr[i], f64::MAX)
                }
            }
            (Some(w), None) => {
                // Resampled along the tempo map until the stretch is ready.
                let tick = view.tick_at_secs(t0 + k as f64 / sr);
                let local = if clip.cycle > 0 {
                    (tick - start + (clip.phase % clip.cycle) as f64).rem_euclid(clip.cycle as f64)
                } else {
                    tick - start
                };
                let beat = warp_origin + local / PPQ as f64;
                let mut pos = w.map.secs_at(beat) * src_rate;
                if clip.looped {
                    pos = pos.rem_euclid(len as f64);
                }
                if pos < 0.0 || pos >= len as f64 {
                    (0.0, 0.0, f64::MAX)
                } else {
                    let i = pos as usize;
                    let j = (i + 1).min(last);
                    let frac = (pos - i as f64) as f32;
                    let l = at(&sample.left, i) + (at(&sample.left, j) - at(&sample.left, i)) * frac;
                    let r = at(right, i) + (at(right, j) - at(right, i)) * frac;
                    (l, r, f64::MAX)
                }
            }
            // Past the file's end, a cycling clip is silent until its next cycle.
            (None, _) if cycle > 0.0 && !clip.looped && p >= len as f64 => (0.0, 0.0, f64::MAX),
            (None, _) => {
                if p >= len as f64 {
                    if !clip.looped {
                        ended = true;
                        break;
                    }
                    p -= len as f64;
                }
                if p < 0.0 {
                    // Crossfade reaching before the file start.
                    p += ratio;
                    (0.0, 0.0, f64::MAX)
                } else {
                    let i = (p as usize).min(last);
                    let j = if clip.looped && i == last { 0 } else { (i + 1).min(last) };
                    let frac = (p - i as f64) as f32;
                    let l = at(&sample.left, i) + (at(&sample.left, j) - at(&sample.left, i)) * frac;
                    let r = at(right, i) + (at(right, j) - at(right, i)) * frac;
                    let until = if clip.looped { f64::MAX } else { (len as f64 - p) / ratio };
                    p += ratio;
                    (l, r, until)
                }
            }
        };
        // En échantillons de sortie ; +1 : le premier échantillon n'est pas nul.
        let since_start = since.min(from_start) + 1.0;
        let until_end = (end_at - k as f64).min(until_src).min(until_cycle);
        let jump = fade_shape((run + 1.0) / fade_in, 0.0);
        let fin = if xin > 0.0 {
            equal_power((k as f64 - win_start) / xin) * jump
        } else if clip.fade_in < 0.0 {
            jump
        } else {
            fade_shape(since_start.min(run + 1.0) / fade_in, clip.fade_in_curve)
        };
        let fout = if xout > 0.0 {
            equal_power((win_end - k as f64) / xout) * fade_shape(until_src / fade_out, 0.0)
        } else if clip.fade_out < 0.0 {
            fade_shape(until_src / fade_out, 0.0)
        } else {
            fade_shape(until_end / fade_out, clip.fade_out_curve)
        };
        let g = clip.gain * (fin * fout).max(0.0) as f32;
        tl[k] += l * g;
        tr[k] += r * g;
        run += 1.0;
    }
    AudioVoice { active: k1 == n && !ended, src: p, run }
}

/// Plays audio clips offline as the engine does in song mode, without the
/// mixer: all clips summed into one stereo buffer of `frames` samples from
/// `from` seconds. Renders a comp exactly as it plays (flattening).
pub fn render_audio_clips(clips: &[AudioClip], tempo: &TempoMap, sample_rate: f32, from: f64, frames: usize) -> (Vec<f32>, Vec<f32>) {
    let (mut left, mut right) = (vec![0.0f32; frames], vec![0.0f32; frames]);
    let mut voices = vec![AudioVoice::default(); clips.len()];
    let view = tempo.view();
    let sr = sample_rate.max(1.0) as f64;
    let mut done = 0;
    while done < frames {
        let n = (frames - done).min(MAX_BLOCK);
        let t0 = from + done as f64 / sr;
        let block = ClipBlock { view: &view, pos_start: view.tick_at_secs(t0), t0, n, sr, contiguous: true };
        for (clip, voice) in clips.iter().zip(&mut voices) {
            let (l, r) = (&mut left[done..done + n], &mut right[done..done + n]);
            *voice = play_audio_clip(clip, *voice, &block, || (l, r));
        }
        done += n;
    }
    (left, right)
}

impl Engine {
    pub fn new(sample_rate: f32, mut channels: Vec<Channel>, sequence: Sequence) -> (Engine, EngineHandle) {
        let bpm = sequence.tempo_view().bpm_at(0.0);
        for ch in &mut channels {
            ch.instrument.set_sample_rate(sample_rate);
            ch.instrument.set_tempo(bpm as f32);
            for fx in ch.note_fx.iter_mut().flatten() {
                fx.set_sample_rate(sample_rate);
                fx.set_tempo(bpm as f32);
            }
        }
        channels.reserve(MAX_CHANNELS.saturating_sub(channels.len()));
        let (tx, rx) = ring::channel(COMMAND_QUEUE);
        let (scope_tx, scope_rx) = ring::channel(SCOPE_QUEUE);
        let (trash_tx, trash_rx) = ring::channel(GARBAGE_QUEUE);
        let state = Arc::new(SharedState {
            playing: AtomicBool::new(false),
            position: AtomicU64::new(0f64.to_bits()),
            count_in: AtomicU64::new(0f64.to_bits()),
            channel_peaks: (0..MAX_CHANNELS).map(|_| PeakMeter::default()).collect(),
            track_peaks: (0..MAX_TRACKS).map(|_| PeakMeter::default()).collect(),
            input_peak: PeakMeter::default(),
            input_clip: AtomicBool::new(false),
            input_gate: AtomicU32::new(1f32.to_bits()),
            load: AtomicU32::new(0),
            latency: AtomicU32::new(0),
            audition: AtomicU64::new((-1f64).to_bits()),
            track_cpu: (0..MAX_TRACKS).map(|_| AtomicU32::new(0)).collect(),
            insert_cpu: (0..MAX_TRACKS * INSERT_SLOTS).map(|_| AtomicU32::new(0)).collect(),
            channel_cpu: (0..MAX_CHANNELS).map(|_| AtomicU32::new(0)).collect(),
            insert_sleep: (0..MAX_TRACKS).map(|_| AtomicU8::new(0)).collect(),
            workers: AtomicU32::new(0),
            parallel: AtomicBool::new(false),
            record_room: std::array::from_fn(|_| AtomicU64::new(0)),
        });
        let (voice, voice_reports) = VoiceInput::new(sample_rate, MAX_BLOCK);
        let mut voices = Vec::with_capacity(MAX_AUDIO_VOICES);
        voices.resize(sequence.audio_clips.len().min(MAX_AUDIO_VOICES), AudioVoice::default());
        let engine = Engine {
            sample_rate,
            channels,
            mixer: Mixer::new(MAX_BLOCK, max_latency(sample_rate)),
            sequence: Box::new(sequence),
            commands: rx,
            state: state.clone(),
            scope: scope_tx,
            scope_phase: 0,
            garbage: trash_tx,
            clock: 0,
            playing: false,
            bpm,
            pos: 0.0,
            anchor_secs: 0.0,
            anchor_clock: 0,
            metronome: false,
            held: [None; MAX_HELD],
            count_in: 0.0,
            count_len: 0.0,
            click_phase: 0.0,
            click_env: 0.0,
            click_hz: 0.0,
            events: Vec::with_capacity(MAX_EVENTS),
            ctl_events: Vec::with_capacity(MAX_EVENTS),
            event_frames: Vec::with_capacity(MAX_EVENTS),
            scratch_l: vec![0.0; MAX_BLOCK],
            scratch_r: vec![0.0; MAX_BLOCK],
            voices,
            audio_next: f64::NAN,
            input: None,
            input_l: vec![0.0; MAX_BLOCK],
            input_r: vec![0.0; MAX_BLOCK],
            strip: InputChain::new(sample_rate),
            low_latency: false,
            smart_disable: false,
            recordings: std::array::from_fn(|_| None),
            calibration: None,
            cal_clock: 0,
            capture: None,
            returning: Vec::with_capacity(2 * MAX_RECORDINGS + 3),
            audition: None,
            midi: MidiLink::new(sample_rate),
            midi_clock: MidiClock::default(),
            pool: None,
            parallelism: None,
            parallel_on: true,
            lanes_l: Vec::new(),
            lanes_r: Vec::new(),
            voice,
        };
        let handle = EngineHandle {
            commands: tx,
            state,
            scope: scope_rx,
            garbage: trash_rx,
            recordings: Vec::new(),
            calibration: None,
            capture: None,
            sample_rate,
            voice: voice_reports,
        };
        (engine, handle)
    }

    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    /// Répartit le calcul sur `workers` threads en plus du thread audio (0 :
    /// tout sur le thread audio). Crée ou arrête des threads : à appeler avant
    /// de confier le moteur au thread audio, jamais depuis celui-ci. Le rendu
    /// est identique au bit près quel que soit le nombre de threads.
    pub fn set_worker_threads(&mut self, workers: usize) {
        self.pool = None;
        let pool = (workers > 0).then(|| WorkerPool::new(workers)).filter(|p| p.workers() > 0);
        let lanes = if pool.is_some() { MAX_CHANNELS * MAX_BLOCK } else { 0 };
        self.lanes_l = vec![0.0; lanes];
        self.lanes_r = vec![0.0; lanes];
        self.state.workers.store(pool.as_ref().map_or(0, |p| p.workers() as u32), Ordering::Relaxed);
        self.pool = pool;
        self.publish_parallel();
    }

    /// Threads de calcul adaptatifs : tant que le thread audio suffit (charge
    /// sous ~40 % du budget), il rend seul et les threads dorment sans rien
    /// consommer ; ils reprennent dès que la charge monte, et se rendorment
    /// après quelques secondes calmes. Sans effet sur le son (identique au bit
    /// près). Désactivé : les threads servent à chaque bloc (export).
    pub fn set_adaptive_threads(&mut self, adaptive: bool) {
        self.parallelism = adaptive.then(Parallelism::default);
        self.parallel_on = !adaptive;
        self.publish_parallel();
    }

    /// Impose l'usage (ou non) des threads de calcul aux prochains blocs, sans
    /// choix adaptatif : pour vérifier que le son n'en dépend pas.
    #[doc(hidden)]
    pub fn force_parallel(&mut self, on: bool) {
        self.parallelism = None;
        self.parallel_on = on;
        self.publish_parallel();
    }

    fn publish_parallel(&self) {
        let active = self.pool.is_some() && self.parallel_on;
        self.state.parallel.store(active, Ordering::Relaxed);
    }

    /// Threads de calcul en plus du thread audio.
    pub fn worker_threads(&self) -> usize {
        self.pool.as_ref().map_or(0, WorkerPool::workers)
    }

    /// Channels, to inspect them before the engine goes to the audio thread.
    pub fn channels(&self) -> &[Channel] {
        &self.channels
    }

    /// Captures `taps` from now on, in place of the previous ones (offline
    /// renders only: the captures grow as they go). A channel or a track
    /// appears once: its last tap wins.
    pub fn set_taps(&mut self, taps: &[Tap]) {
        for ch in &mut self.channels {
            ch.tap = None;
        }
        for t in &mut self.mixer.tracks {
            t.tap = None;
        }
        for &tap in taps {
            let buf = Some(Vec::with_capacity(1 << 16));
            match tap {
                Tap::Channel(c) | Tap::ChannelOut(c) => {
                    if let Some(ch) = self.channels.get_mut(c) {
                        ch.tap = buf;
                        ch.tap_out = matches!(tap, Tap::ChannelOut(_));
                    }
                }
                Tap::Track(t) => {
                    if let Some(t) = self.mixer.tracks.get_mut(t) {
                        t.tap = buf;
                    }
                }
            }
        }
    }

    /// What each tap captured since the last call (interleaved stereo), in
    /// the order of `taps`.
    pub fn take_taps(&mut self, taps: &[Tap]) -> Vec<Vec<f32>> {
        taps.iter()
            .map(|&tap| {
                let buf = match tap {
                    Tap::Channel(c) | Tap::ChannelOut(c) => self.channels.get_mut(c).and_then(|ch| ch.tap.as_mut()),
                    Tap::Track(t) => self.mixer.tracks.get_mut(t).and_then(|t| t.tap.as_mut()),
                };
                buf.map(std::mem::take).unwrap_or_default()
            })
            .collect()
    }

    /// Smart disable of the inserts: those fed silence that give silence back
    /// sleep until sound comes in again ([`Mixer::set_smart_disable`]). Off by
    /// default, so that offline renders process everything.
    pub fn set_smart_disable(&mut self, on: bool) {
        self.smart_disable = on;
        self.mixer.set_smart_disable(on, self.sample_rate);
    }

    fn wake_insert(&mut self, track: usize, slot: usize) {
        if let Some(t) = self.mixer.tracks.get_mut(track) {
            t.wake(slot);
        }
    }

    /// Accès direct au mixer, pour préparer l'état avant le premier bloc.
    pub fn mixer_mut(&mut self) -> &mut Mixer {
        &mut self.mixer
    }

    /// Replaces the mixer by an empty one of `count` tracks, master included
    /// (allocates: before the engine goes to the audio thread).
    pub fn set_track_count(&mut self, count: usize) {
        self.mixer = Mixer::with_tracks(count, MAX_BLOCK, max_latency(self.sample_rate));
        self.mixer.set_smart_disable(self.smart_disable, self.sample_rate);
    }

    /// The new mixer takes over the kept tracks; everything that points at a
    /// track follows it. Without allocation.
    fn remap_tracks(&mut self, mut remap: Box<TrackRemap>) {
        let TrackRemap { mixer, from, to } = &mut *remap;
        mixer.adopt(&mut self.mixer, from, to);
        std::mem::swap(&mut self.mixer, mixer);
        let count = self.mixer.len();
        let map = |t: usize| to.get(t).copied().filter(|&x| x < count);
        for ch in &mut self.channels {
            ch.track = map(ch.track).unwrap_or(MASTER);
        }
        for held in &mut self.held {
            *held = held.and_then(|target| match target {
                AutoTarget::EffectParam { track, slot, id } => map(track).map(|track| AutoTarget::EffectParam { track, slot, id }),
                AutoTarget::TrackGain(t) => map(t).map(AutoTarget::TrackGain),
                AutoTarget::TrackPan(t) => map(t).map(AutoTarget::TrackPan),
                other => Some(other),
            });
        }
        // A take on a deleted track ends; the others follow their track.
        for k in 0..MAX_RECORDINGS {
            let Some(rec) = self.recordings[k].as_mut() else { continue };
            let source = match rec.source.resampled() {
                Some(s) => map(s).map(InputSource::Resample),
                None => Some(rec.source),
            };
            match map(rec.track).zip(source) {
                Some((t, s)) => (rec.track, rec.source) = (t, s),
                None => {
                    if let Some(rec) = self.recordings[k].take() {
                        self.trash(Garbage::Recording(rec));
                    }
                }
            }
        }
        self.publish_record_room();
        self.trash(Garbage::Mixer(remap));
    }

    /// Samples per tick at the current tempo (count-in, recording).
    fn spt(&self) -> f64 {
        self.sample_rate as f64 * 60.0 / (self.bpm * PPQ as f64)
    }

    /// Seconds on the tempo map at the start of the next block.
    fn block_secs(&self) -> f64 {
        self.anchor_secs + (self.clock - self.anchor_clock) as f64 / self.sample_rate as f64
    }

    /// Restarts the position count from here (after a jump, a new tempo map…).
    fn anchor(&mut self) {
        self.anchor_secs = self.sequence.tempo_view().secs_at(self.pos);
        self.anchor_clock = self.clock;
    }

    /// Frames from the start of the next block to `tick`, rounded up.
    /// Tolerance: a position computed a hair before `tick` must not add a sample.
    fn frames_to(&self, tick: f64) -> f64 {
        ((self.sequence.tempo_view().secs_at(tick) - self.block_secs()) * self.sample_rate as f64 - 1e-6).ceil()
    }

    /// Tempo at the current position, passed on to the synced effects.
    fn follow_tempo(&mut self) {
        let bpm = self.sequence.tempo_view().bpm_at(self.pos);
        if (bpm - self.bpm).abs() > 1e-6 {
            self.bpm = bpm;
            self.set_tempo();
        }
    }

    fn trash(&mut self, g: Garbage) {
        // File pleine : l'objet est libéré ici, en dernier recours ; une prise
        // ou un calibrage est gardé pour le bloc suivant s'il y a de la place.
        if let Err(g @ (Garbage::Recording(_) | Garbage::Calibration(_) | Garbage::Capture(_))) = self.garbage.push(g)
            && self.returning.len() < self.returning.capacity()
        {
            self.returning.push(g);
        }
    }

    /// Rend à l'interface l'enregistrement de `track`, ou tous (`None`).
    fn finish_recording(&mut self, track: Option<usize>) {
        for k in 0..MAX_RECORDINGS {
            if self.recordings[k].as_ref().is_some_and(|r| track.is_none_or(|t| r.track == t))
                && let Some(rec) = self.recordings[k].take()
            {
                self.trash(Garbage::Recording(rec));
            }
        }
        self.publish_record_room();
    }

    /// Remplit `out` (entrelacé, `channels` canaux) — appelé par le thread audio.
    pub fn process(&mut self, out: &mut [f32], channels: usize) {
        self.run(out, channels, false, &[], 0);
    }

    /// Duplex intégral : comme [`Engine::process`], avec l'entrée captée dans le
    /// même cycle (`input` entrelacée, `in_channels` canaux, autant de trames
    /// que `out` ; 0 canal : pas d'entrée, la file de [`Command::SetInput`]
    /// sert alors). L'écoute n'ajoute ainsi aucun retard au pilote.
    pub fn process_duplex(&mut self, input: &[f32], in_channels: usize, out: &mut [f32], channels: usize) {
        self.run(out, channels, false, input, in_channels);
    }

    /// Comme [`Engine::process`], mais s'arrête pile à la fin de la boucle du
    /// morceau au lieu de reboucler (export) ; renvoie le nombre d'images écrites,
    /// le reste de `out` n'est pas touché.
    pub fn process_to_loop_end(&mut self, out: &mut [f32], channels: usize) -> usize {
        self.run(out, channels, true, &[], 0)
    }

    /// Pistes dont la latence est réduite : celles écoutées et leur trajet jusqu'au master.
    fn plan_monitoring(&mut self) {
        let Mixer { tracks, reduced, monitor, .. } = &mut self.mixer;
        reduced.fill(false);
        if self.low_latency {
            let count = tracks.len();
            for t in (0..count).filter(|&t| monitor[t].is_some_and(|s| s.resampled().is_none())) {
                let mut cur = t;
                while !reduced[cur] {
                    reduced[cur] = true;
                    let out = tracks[cur].output;
                    cur = if cur == MASTER { MASTER } else if out < count && out != cur { out } else { MASTER };
                }
            }
        }
    }

    fn run(&mut self, out: &mut [f32], channels: usize, stop_at_end: bool, input: &[f32], in_channels: usize) -> usize {
        let started = Instant::now();
        // Ringing tails would otherwise fall into slow denormal arithmetic.
        let _flush = crate::FlushDenormals::new();
        // Repère de temps avant les commandes : les notes jouées en direct
        // (et les coupures) sont datées du début de ce bloc.
        self.midi.begin(self.clock, self.mixer.latency());
        self.drain_commands();
        self.plan_monitoring();
        let channels = channels.max(1);
        let frames = out.len() / channels;
        let mut done = 0;
        while done < frames {
            let mut n = (frames - done).min(MAX_BLOCK);
            if self.playing {
                let end = self.sequence.loop_end();
                if let Some(end) = end
                    && (self.pos >= end - 1e-9 || self.frames_to(end) <= 0.0)
                {
                    if stop_at_end {
                        break;
                    }
                    self.pos = self.sequence.loop_start();
                    self.anchor();
                }
                self.apply_automation();
                if let Some(end) = end {
                    // On coupe le bloc à la fin de la boucle du morceau.
                    n = n.min(self.frames_to(end).max(1.0) as usize);
                }
            } else if self.count_in > 0.0 {
                // Le bloc s'arrête pile à la fin du pré-compte.
                let left = (self.count_in * self.spt() - 1e-6).ceil().max(1.0) as usize;
                n = n.min(left);
            }
            self.follow_tempo();
            let direct = if in_channels > 0 { input.get(done * in_channels..).unwrap_or(&[]) } else { &[] };
            self.process_chunk(n, direct, in_channels);
            let (l, r) = self.mixer.master(n);
            let block = &mut out[done * channels..(done + n) * channels];
            write_interleaved(block, channels, l, r);
            if let Some(voice) = self.audition.as_mut()
                && !voice.render_add(block, channels, self.sample_rate)
                && let Some(done) = self.audition.take()
            {
                self.trash(Garbage::Audition(done));
            }
            done += n;
        }
        let heard = self.audition.as_ref().map_or(-1.0, |a| a.position());
        self.state.audition.store(heard.to_bits(), Ordering::Relaxed);
        self.state.position.store(self.pos.to_bits(), Ordering::Relaxed);
        self.state.latency.store(self.mixer.latency() as u32, Ordering::Relaxed);
        self.state.count_in.store(self.count_in.to_bits(), Ordering::Relaxed);
        self.state.playing.store(self.playing, Ordering::Relaxed);
        self.state.input_gate.store(self.strip.gate_gain().to_bits(), Ordering::Relaxed);
        if frames > 0 {
            let budget = frames as f32 / self.sample_rate;
            let wall = started.elapsed();
            let load = wall.as_secs_f32() / budget;
            self.state.load.fetch_max(load.to_bits(), Ordering::Relaxed);
            // Charge qu'aurait eue le thread audio seul : le temps des tâches
            // remplace celui des travaux partagés.
            let (work, jobs) = self.pool.as_ref().map_or((0, 0), WorkerPool::take_times);
            let serial = (wall.as_nanos() as u64 + work).saturating_sub(jobs) as f32 * 1e-9;
            if let Some(p) = self.parallelism.as_mut() {
                let on = p.update(serial / budget, budget);
                if on != self.parallel_on {
                    self.parallel_on = on;
                    self.publish_parallel();
                }
            }
        }
        self.publish_cpu(done);
        done
    }

    /// Charge lissée de chaque piste, insert et canal sur les `frames` images
    /// rendues : temps mesuré / durée réelle de ces images.
    fn publish_cpu(&mut self, frames: usize) {
        if frames == 0 {
            return;
        }
        let secs = frames as f32 / self.sample_rate.max(1.0);
        let alpha = (secs / CPU_SMOOTHING_SECONDS).min(1.0);
        let per_ns = 1e-9 / secs;
        let smooth = |cell: &AtomicU32, ns: u64| {
            let old = f32::from_bits(cell.load(Ordering::Relaxed));
            let new = old + (ns as f32 * per_ns - old) * alpha;
            cell.store(new.to_bits(), Ordering::Relaxed);
        };
        let state = &*self.state;
        self.mixer.take_cpu(|idx, track, inserts, asleep| {
            smooth(&state.track_cpu[idx], track);
            if let Some(cell) = state.insert_sleep.get(idx) {
                cell.store(asleep, Ordering::Relaxed);
            }
            for (slot, &ns) in inserts.iter().enumerate() {
                smooth(&state.insert_cpu[idx * INSERT_SLOTS + slot], ns);
            }
        });
        for (idx, cell) in state.channel_cpu.iter().enumerate() {
            let ns = self.channels.get_mut(idx).map_or(0, |ch| std::mem::take(&mut ch.cpu_ns));
            smooth(cell, ns);
        }
    }

    /// Applique les courbes actives à la position courante (début de bloc).
    /// Tempo lanes are folded into the tempo map by the project.
    fn apply_automation(&mut self) {
        let pos = self.pos;
        for lane in &self.sequence.automation {
            if self.held.contains(&Some(lane.target)) {
                continue;
            }
            let Some(v) = self.sequence.automation_tick(lane.scope, pos).and_then(|t| lane.value_at(t)) else {
                continue;
            };
            if !v.is_finite() {
                continue;
            }
            match lane.target {
                // Sent on their sample by `process_chunk`.
                AutoTarget::InstrumentParam { .. } | AutoTarget::PitchBend(_) | AutoTarget::Cc { .. } => {}
                AutoTarget::EffectParam { track, slot, id } => {
                    if let Some(t) = self.mixer.tracks.get_mut(track)
                        && !t.mods.as_mut().is_some_and(|m| m.set_base(slot, id, v))
                        && let Some(Some(fx)) = t.inserts.get_mut(slot)
                        && fx.get_param(id) != v
                    {
                        fx.set_param(id, v);
                    }
                }
                AutoTarget::ChannelGain(c) => {
                    if let Some(ch) = self.channels.get_mut(c) {
                        ch.gain = v.max(0.0);
                    }
                }
                AutoTarget::ChannelPan(c) => {
                    if let Some(ch) = self.channels.get_mut(c) {
                        ch.pan = v.clamp(-1.0, 1.0);
                    }
                }
                AutoTarget::TrackGain(t) => {
                    if let Some(t) = self.mixer.tracks.get_mut(t) {
                        t.gain = v.max(0.0);
                    }
                }
                AutoTarget::TrackPan(t) => {
                    if let Some(t) = self.mixer.tracks.get_mut(t) {
                        t.pan = v.clamp(-1.0, 1.0);
                    }
                }
                AutoTarget::Tempo => {}
            }
        }
    }

    /// Events of the lanes sent on their sample ([`AutoTarget::is_event`])
    /// in the block that starts at tick `from`: a value on each tick where
    /// it changes, and the value at `from` when the block does not follow
    /// the previous one. `frame_in` gives the frame of a tick in the block,
    /// `None` outside: a tick on the edge of two blocks belongs to one only.
    fn collect_controls(
        seq: &Sequence,
        held: &[Option<AutoTarget>],
        out: &mut Vec<(usize, usize, f32)>,
        (from, to): (f64, f64),
        restart: bool,
        frame_in: &dyn Fn(f64) -> Option<usize>,
    ) {
        out.clear();
        for (li, lane) in seq.automation.iter().enumerate() {
            if !lane.target.is_event() || held.contains(&Some(lane.target)) {
                continue;
            }
            let value_at = |tick: f64| seq.automation_tick(lane.scope, tick).and_then(|t| lane.value_at(t)).filter(|v| v.is_finite());
            let full = |events: &Vec<(usize, usize, f32)>| events.len() == events.capacity();
            if restart && let Some(v) = value_at(from) && !full(out) {
                out.push((0, li, v));
            }
            let mut t = from.floor();
            while t <= to.ceil() && !full(out) {
                let cur = value_at(t);
                if let Some(frame) = frame_in(t)
                    && !(restart && frame == 0)
                    && let Some(v) = cur
                    && cur != value_at(t - 1.0)
                {
                    out.push((frame, li, v));
                }
                t += 1.0;
            }
        }
        out.sort_unstable_by_key(|e| e.0);
    }

    /// Sends a controller event of automation lane `lane`.
    fn apply_control(&mut self, lane: usize, v: f32) {
        let Some(target) = self.sequence.automation.get(lane).map(|l| l.target) else { return };
        let Some(ch) = target.channel().and_then(|c| self.channels.get_mut(c)) else { return };
        match target {
            AutoTarget::InstrumentParam { id, .. } => {
                if !ch.mods.as_mut().is_some_and(|m| m.set_base(0, id, v)) && ch.instrument.get_param(id) != v {
                    ch.instrument.set_param(id, v);
                }
            }
            AutoTarget::PitchBend(_) => ch.instrument.controller(crate::Controller::PitchBend(v.clamp(-1.0, 1.0))),
            AutoTarget::Cc { cc, .. } => ch.instrument.controller(crate::Controller::Cc(cc.min(127), v.clamp(0.0, 1.0))),
            _ => {}
        }
    }

    /// The effect rack in an insert slot.
    fn rack(&mut self, track: usize, slot: usize) -> Option<&mut crate::effects::EffectRack> {
        self.mixer.tracks.get_mut(track)?.inserts.get_mut(slot)?.as_mut()?.as_rack()
    }

    /// Transport for the modulators, at the start of the block.
    fn mod_clock(&self) -> ModClock {
        ModClock { sample_rate: self.sample_rate, bpm: self.bpm, beats: self.pos / PPQ as f64, playing: self.playing }
    }

    /// Channel modulators: one update per block, before the instruments play.
    fn modulate_channels(&mut self, n: usize) {
        let clock = self.mod_clock();
        for ch in &mut self.channels {
            let Channel { mods, instrument, level, .. } = ch;
            if let Some(mods) = mods.as_mut().filter(|m| !m.is_empty()) {
                mods.run(&clock, n, &ModInput::Level(*level), |_, id, v| instrument.set_param(id, v));
            }
            *level = 0.0;
        }
    }

    /// Transmet le tempo courant aux effets synchronisés.
    fn set_tempo(&mut self) {
        let bpm = self.bpm as f32;
        self.mixer.set_tempo(bpm);
        for ch in &mut self.channels {
            ch.set_tempo(bpm);
        }
    }

    fn process_chunk(&mut self, n: usize, input: &[f32], in_channels: usize) {
        let spt = self.spt();
        let sr = self.sample_rate as f64;
        let pos_start = self.pos;
        let t0 = self.block_secs();
        let view = self.sequence.tempo_view();
        let to = view.tick_at_secs(t0 + n as f64 / sr);
        // Frame of a tick in this block: the first sample at or after it.
        let frame_of = |tick: f64| (((view.secs_at(tick) - t0) * sr - 1e-6).ceil().max(0.0) as usize).min(n - 1);
        self.event_frames.clear();
        if self.playing {
            self.sequence.collect(pos_start, to, &mut self.events);
            for ev in &self.events {
                let len = (view.secs_at(ev.tick + ev.len) - view.secs_at(ev.tick)) * sr;
                self.event_frames.push((frame_of(ev.tick), len.round().max(1.0) as u64));
            }
        } else {
            self.events.clear();
        }
        let contiguous = (pos_start - self.audio_next).abs() < 1e-6;
        if self.playing {
            let frame_in = |tick: f64| {
                let f = ((view.secs_at(tick) - t0) * sr - 1e-6).ceil();
                (f >= 0.0 && f < n as f64).then_some(f as usize)
            };
            Self::collect_controls(&self.sequence, &self.held, &mut self.ctl_events, (pos_start, to), !contiguous, &frame_in);
        } else {
            self.ctl_events.clear();
        }
        let freeze = FreezeClock { playing: self.playing, song: self.sequence.mode == PlayMode::Song, contiguous, secs: t0, sample_rate: sr };
        for f in self.channels.iter_mut().filter_map(|ch| ch.frozen.as_mut()) {
            f.sync(&freeze);
        }
        self.mixer.freeze_clock = freeze;
        self.midi_clock.process(&mut self.midi, self.playing, pos_start, contiguous, n, to, &frame_of, self.clock);

        // The input is read before the channels play: the voice's notes
        // start in this block, on their sample.
        let has_input = self.read_input(input, in_channels, n);
        self.voice.analyze(has_input.then(|| (&self.input_l[..n], &self.input_r[..n])));
        let view = self.sequence.tempo_view();
        for e in &mut self.voice.events {
            if self.playing {
                e.2 = view.tick_at_secs(t0 + e.0 as f64 / sr);
            }
        }

        self.mixer.clear(n);
        self.modulate_channels(n);
        self.mixer.clock = self.mod_clock();
        // Threads de calcul : rendus à la fin du bloc (déplacer le pool n'alloue rien).
        // Endormis quand le thread audio suffit (choix adaptatif).
        let pool = self.pool.take();
        let active = pool.as_ref().filter(|_| self.parallel_on);
        let parallel = active.filter(|_| self.channels.len() >= 2);
        let mut i = 0;
        let mut next_event = 0;
        let mut next_ctl = 0;
        let mut next_voice = 0;
        while i < n {
            let now = self.clock + i as u64;
            // Controllers first: a bend on a note's start applies to it.
            while let Some(&(frame, lane, v)) = self.ctl_events.get(next_ctl) {
                if frame > i {
                    break;
                }
                next_ctl += 1;
                self.apply_control(lane, v);
            }
            // Events due now: slides first, so that the notes ending here
            // still glide (legato), then the note ends, then the new notes.
            let due = next_event + self.event_frames[next_event..].iter().take_while(|&&(frame, _)| frame <= i).count();
            for k in next_event..due {
                let (ev, len) = (self.events[k], self.event_frames[k].1);
                if ev.slide
                    && let Some(ch) = self.channels.get_mut(ev.channel)
                    && !ch.muted
                    && ch.slide(now, f32::from(ev.key) + ev.fine / 100.0, len as f32 / self.sample_rate, now + len)
                {
                    self.events[k].vel = -1.0;
                }
            }
            for ch in &mut self.channels {
                ch.release_due(now);
            }
            for k in next_event..due {
                let (ev, len) = (self.events[k], self.event_frames[k].1);
                // A slide that glided is done; one with nothing to glide plays.
                if ev.vel < 0.0 {
                    continue;
                }
                if let Some(ch) = self.channels.get_mut(ev.channel)
                    && !ch.muted
                {
                    ch.play(now, ev.key, ev.vel, now + len, ev.pan, ev.fine);
                }
            }
            next_event = due;
            // Notes of the voice, like a controller's.
            while let Some(&(frame, ev, tick)) = self.voice.events.get(next_voice) {
                if frame > i {
                    break;
                }
                next_voice += 1;
                self.voice.apply(ev, tick, &mut self.channels, now);
            }
            for ch in &mut self.channels {
                ch.flush(now);
            }
            // Jusqu'au prochain événement (début ou fin de note).
            let mut end = n;
            if let Some(&(frame, _)) = self.event_frames.get(next_event) {
                end = end.min(frame.clamp(i + 1, n));
            }
            if let Some(&(frame, _, _)) = self.ctl_events.get(next_ctl) {
                end = end.min(frame.clamp(i + 1, n));
            }
            if let Some(&(frame, _, _)) = self.voice.events.get(next_voice) {
                end = end.min(frame.clamp(i + 1, n));
            }
            let next_off = self.channels.iter().filter_map(Channel::next_off).min();
            let next_fx = self.channels.iter().filter_map(Channel::next_fx_event).min();
            if let Some(at) = next_off.into_iter().chain(next_fx).min() {
                end = end.min((at.saturating_sub(self.clock) as usize).clamp(i + 1, n));
            }
            match parallel {
                Some(pool) => self.render_channels_parallel(pool, i, end - i),
                None => self.render_channels(i, end - i),
            }
            for ch in &mut self.channels {
                drain_midi(&mut self.midi, &mut *ch.instrument, now);
            }
            i = end;
        }
        if parallel.is_some() {
            self.sum_lanes(n);
        }

        self.render_audio_clips(pos_start, n, t0);
        if has_input {
            self.route_input(pos_start, n, spt);
        }
        // Resampling the master: the click joins after the take, unrecorded.
        let resampling = self.recordings.iter().flatten().any(|r| r.source == InputSource::Resample(MASTER));
        if !resampling {
            self.metronome_click(pos_start, n, t0, spt, false);
        }
        for (i, notes) in self.mixer.notes.iter_mut().enumerate() {
            *notes = self.channels.get(i).map_or(crate::ChannelNotes::EMPTY, |c| c.notes);
        }
        match active {
            Some(pool) => self.mixer.process_parallel(n, &self.state.track_peaks, pool),
            None => self.mixer.process(n, &self.state.track_peaks),
        }
        self.pool = pool;
        self.resample(pos_start, n, spt, contiguous);
        if resampling {
            self.metronome_click(pos_start, n, t0, spt, true);
        }
        self.calibration_clicks(n);
        self.feed_scope(n);
        self.clock += n as u64;
        if self.playing {
            self.pos = self.sequence.tempo_view().tick_at_secs(self.block_secs());
            self.audio_next = self.pos;
        } else {
            self.audio_next = f64::NAN;
        }
        if self.count_in > 0.0 {
            self.count_in -= n as f64 / spt;
            if self.count_in <= 1e-6 {
                self.count_in = 0.0;
                self.playing = true;
                self.anchor();
            }
        }
    }

    /// Clips audio (mode morceau), début à l'échantillon près, fondus aux bords.
    /// Unwarped clips keep their place in ticks and play at their own speed
    /// (linear interpolation): they don't follow tempo changes. Warped clips
    /// play their rendered stretch, or until it is ready their file resampled
    /// along the tempo map; both are computed from the time of each sample, so
    /// a clip is always in time, whatever the block size.
    fn render_audio_clips(&mut self, pos_start: f64, n: usize, t0: f64) {
        let song = self.playing && !matches!(self.sequence.mode, PlayMode::Pattern(_));
        // Bloc dans la continuité du précédent : les clips reprennent où ils étaient
        // (même si le tempo a changé entre-temps).
        let contiguous = (pos_start - self.audio_next).abs() < 1e-6;
        let sr = self.sample_rate as f64;
        let view = self.sequence.tempo_view();
        let block = ClipBlock { view: &view, pos_start, t0, n, sr, contiguous };
        for (idx, clip) in self.sequence.audio_clips.iter().enumerate() {
            let prev = self.voices.get(idx).copied().unwrap_or_default();
            let mixer = &mut self.mixer;
            let voice = if song { play_audio_clip(clip, prev, &block, || mixer.input(clip.track, n)) } else { AudioVoice::default() };
            if let Some(v) = self.voices.get_mut(idx) {
                *v = voice;
            }
        }
    }

    /// Lit l'entrée (directe en duplex, sinon la file) : calibrage sur l'entrée
    /// brute, tranche d'entrée, crête. Faux sans entrée.
    fn read_input(&mut self, direct: &[f32], in_channels: usize, n: usize) -> bool {
        let (l, r) = (&mut self.input_l[..n], &mut self.input_r[..n]);
        if in_channels > 0 {
            deinterleave(direct, in_channels, l, r);
        } else if let Some(input) = self.input.as_mut() {
            input.read(l, r);
        } else {
            return false;
        }
        if let Some(cal) = self.calibration.as_mut() {
            cal.write(l, r);
            if cal.is_full()
                && let Some(cal) = self.calibration.take()
            {
                self.trash(Garbage::Calibration(cal));
            }
        }
        let (l, r) = (&mut self.input_l[..n], &mut self.input_r[..n]);
        if self.strip.process(l, r) {
            self.state.input_clip.store(true, Ordering::Relaxed);
        }
        self.state.input_peak.record(peak(l), peak(r));
        if let Some(cap) = self.capture.as_mut() {
            cap.write(l, r);
            if cap.is_full()
                && let Some(cap) = self.capture.take()
            {
                self.trash(Garbage::Capture(cap));
            }
        }
        true
    }

    /// Écoute des pistes, puis prises si le transport tourne.
    fn route_input(&mut self, pos_start: f64, n: usize, spt: f64) {
        for track in 0..self.mixer.len() {
            let Some(source) = self.mixer.monitor[track].filter(|s| s.resampled().is_none()) else { continue };
            let (sl, sr) = source.pick(&self.input_l[..n], &self.input_r[..n]);
            let (tl, tr) = self.mixer.input(track, n);
            for k in 0..n {
                tl[k] += sl[k];
                tr[k] += sr[k];
            }
        }
        if !self.playing {
            return;
        }
        // Bloc dans la continuité du précédent ? Sinon (boucle, saut), la prise le note.
        let contiguous = (pos_start - self.audio_next).abs() < 1e-6;
        // Une prise qui commence lit la latence du mixer tel qu'il est maintenant
        // (inserts tout juste changés compris), pas celle du bloc précédent.
        if self.recordings.iter().flatten().any(|r| r.len == 0 && r.source.resampled().is_none()) {
            self.mixer.plan_latency();
        }
        let latency = self.mixer.latency();
        for rec in self.recordings.iter_mut().flatten().filter(|r| r.source.resampled().is_none()) {
            if rec.len == 0 {
                rec.start_tick = pos_start;
                rec.mix_latency = latency;
                rec.samples_per_tick = spt;
                rec.jumps.clear();
            } else if !contiguous {
                rec.jump(pos_start);
            }
            let (sl, sr) = rec.source.pick(&self.input_l[..n], &self.input_r[..n]);
            // Tampon plein : on cesse d'écrire, il sera rendu au désarmement.
            rec.write(sl, sr);
        }
        self.publish_record_room();
    }

    /// Resampling takes: the post-fader output of their track (or the
    /// master's), as just mixed, while the transport plays. The take notes
    /// that output's latency: it lags the timeline by that much.
    fn resample(&mut self, pos_start: f64, n: usize, spt: f64, contiguous: bool) {
        if !self.playing || !self.recordings.iter().flatten().any(|r| r.source.resampled().is_some()) {
            return;
        }
        for rec in self.recordings.iter_mut().flatten() {
            let Some(track) = rec.source.resampled() else { continue };
            let Some((l, r)) = self.mixer.output(track, n) else { continue };
            if rec.len == 0 {
                rec.start_tick = pos_start;
                rec.mix_latency = self.mixer.output_latency(track);
                rec.samples_per_tick = spt;
                rec.jumps.clear();
            } else if !contiguous {
                rec.jump(pos_start);
            }
            rec.write(l, r);
        }
        self.publish_record_room();
    }

    /// Place libre des prises en cours, pour que l'interface les allonge à temps.
    fn publish_record_room(&self) {
        for (cell, rec) in self.state.record_room.iter().zip(&self.recordings) {
            let v = rec.as_ref().map_or(0, |r| ((r.track as u64 + 1) << 32) | r.room().min(u32::MAX as usize) as u64);
            cell.store(v, Ordering::Relaxed);
        }
    }

    /// Clics du calibrage, joués après le mixer : ni compensation ni effets.
    fn calibration_clicks(&mut self, n: usize) {
        let Some(cal) = self.calibration.as_ref() else {
            self.cal_clock = 0;
            return;
        };
        let sr = cal.sample_rate;
        let start = self.cal_clock;
        let (l, r) = self.mixer.master_mut(n);
        for &at in &CALIBRATION_CLICKS {
            let at = (at * sr) as usize;
            for k in 0..n {
                if let Some(i) = (start + k).checked_sub(at) {
                    let s = calibration_click(i, sr);
                    l[k] += s;
                    r[k] += s;
                }
            }
        }
        self.cal_clock += n;
    }

    /// Rend le canal sur `l` et `r` (remis à zéro), gain, pan et crête compris
    /// (rien de plus qu'avancer l'instrument s'il est coupé).
    fn render_channel(ch: &mut Channel, l: &mut [f32], r: &mut [f32], meter: Option<&PeakMeter>) {
        let started = Instant::now();
        l.fill(0.0);
        r.fill(0.0);
        match ch.frozen.as_mut() {
            Some(f) => f.read(l, r),
            None => ch.instrument.render(l, r),
        }
        if let Some(tap) = ch.tap.as_mut().filter(|_| !ch.tap_out) {
            tap.extend(l.iter().zip(r.iter()).flat_map(|(&a, &b)| [a, b]));
        }
        if !ch.muted {
            let (gl, gr) = balance_gains(ch.pan);
            let (gl, gr) = (gl * ch.gain, gr * ch.gain);
            for s in l.iter_mut() {
                *s *= gl;
            }
            for s in r.iter_mut() {
                *s *= gr;
            }
            if let Some(m) = meter {
                m.record(peak(l), peak(r));
            }
            if ch.mods.is_some() {
                ch.level = ch.level.max(peak(l)).max(peak(r));
            }
        }
        if let Some(tap) = ch.tap.as_mut().filter(|_| ch.tap_out) {
            // A muted channel sends nothing to its track.
            let silent = if ch.muted { 0.0 } else { 1.0 };
            tap.extend(l.iter().zip(r.iter()).flat_map(|(&a, &b)| [a * silent, b * silent]));
        }
        ch.cpu_ns += started.elapsed().as_nanos() as u64;
    }

    /// Chaque canal rendu dans sa voie, sur les threads de calcul.
    fn render_channels_parallel(&mut self, pool: &WorkerPool, offset: usize, len: usize) {
        let channels = SyncPtr(self.channels.as_mut_ptr());
        let (lanes_l, lanes_r) = (SyncPtr(self.lanes_l.as_mut_ptr()), SyncPtr(self.lanes_r.as_mut_ptr()));
        let meters = &self.state.channel_peaks;
        debug_assert!(self.channels.len() * MAX_BLOCK <= self.lanes_l.len() && offset + len <= MAX_BLOCK);
        let task = |idx: usize| {
            let at = idx * MAX_BLOCK + offset;
            // SAFETY: la tâche `idx` est seule à toucher le canal `idx` et sa
            // voie, qui tient dans les tampons (capacité de MAX_CHANNELS canaux).
            let (ch, l, r) = unsafe {
                (
                    &mut *channels.at(idx),
                    std::slice::from_raw_parts_mut(lanes_l.at(at), len),
                    std::slice::from_raw_parts_mut(lanes_r.at(at), len),
                )
            };
            Self::render_channel(ch, l, r, meters.get(idx));
        };
        // Sous-bloc trop court pour amortir la répartition : rendu ici, dans
        // les mêmes voies (le résultat ne dépend pas de ce choix).
        if len < MIN_PARALLEL_FRAMES {
            (0..self.channels.len()).for_each(task);
        } else {
            pool.run(self.channels.len(), &task);
        }
    }

    /// Additionne les voies des canaux dans leurs pistes, dans l'ordre des
    /// canaux : les mêmes additions, dans le même ordre, que [`Engine::render_channels`].
    fn sum_lanes(&mut self, n: usize) {
        for (idx, ch) in self.channels.iter().enumerate() {
            if ch.muted {
                continue;
            }
            let at = idx * MAX_BLOCK;
            let (l, r) = (&self.lanes_l[at..at + n], &self.lanes_r[at..at + n]);
            let (tl, tr) = self.mixer.input(ch.track, n);
            for k in 0..n {
                tl[k] += l[k];
                tr[k] += r[k];
            }
        }
    }

    fn render_channels(&mut self, offset: usize, len: usize) {
        for (idx, ch) in self.channels.iter_mut().enumerate() {
            let (l, r) = (&mut self.scratch_l[..len], &mut self.scratch_r[..len]);
            Self::render_channel(ch, l, r, self.state.channel_peaks.get(idx));
            if ch.muted {
                continue;
            }
            let (tl, tr) = self.mixer.input(ch.track, offset + len);
            for k in 0..len {
                tl[offset + k] += l[k];
                tr[offset + k] += r[k];
            }
        }
    }

    /// Clic de métronome sur chaque temps, ajouté directement au master, with an
    /// accent on the first beat of each bar (beats and bars follow the time
    /// signature map). Pendant le pré-compte, les temps se comptent depuis son
    /// début, in the signature where playback will start.
    /// `after`: on the master's output (past its inserts and fader), not its input.
    fn metronome_click(&mut self, pos_start: f64, n: usize, t0: f64, spt: f64, after: bool) {
        const MAX_CLICKS: usize = 16;
        let mut clicks = [(usize::MAX, false); MAX_CLICKS];
        let mut count = 0;
        let mut push = |frame: usize, down: bool| {
            if frame < n && count < MAX_CLICKS {
                clicks[count] = (frame, down);
                count += 1;
            }
        };
        let view = self.sequence.tempo_view();
        if self.count_in > 0.0 {
            let from = self.count_len - self.count_in;
            let to = from + n as f64 / spt;
            let sig = view.sig_at(self.pos);
            let beat = sig.beat_len() as f64;
            let mut k = (from / beat - 1e-9).ceil().max(0.0) as u64;
            while (k as f64 * beat) < to {
                let frame = ((k as f64 * beat - from) * spt - 1e-6).ceil().max(0.0) as usize;
                push(frame, k % sig.num.max(1) as u64 == 0);
                k += 1;
            }
        } else if self.playing && self.metronome {
            let sr = self.sample_rate as f64;
            let to = view.tick_at_secs(t0 + n as f64 / sr);
            view.for_each_beat(pos_start, to, |tick, down| {
                push(((view.secs_at(tick) - t0) * sr - 1e-6).ceil().max(0.0) as usize, down);
            });
        } else {
            self.click_env = 0.0;
            return;
        }
        let sr = self.sample_rate;
        let decay = (-1.0 / (0.012 * sr)).exp();
        let (l, r) = if after { self.mixer.master_mut(n) } else { self.mixer.input(MASTER, n) };
        let mut next = 0;
        for k in 0..n {
            while next < count && clicks[next].0 <= k {
                self.click_hz = if clicks[next].1 { 1_760.0 } else { 1_320.0 };
                self.click_env = 0.35;
                self.click_phase = 0.0;
                next += 1;
            }
            if self.click_env > 1e-4 {
                self.click_phase = (self.click_phase + self.click_hz / sr).fract();
                let s = (self.click_phase * std::f32::consts::TAU).sin() * self.click_env;
                self.click_env *= decay;
                l[k] += s;
                r[k] += s;
            }
        }
    }

    fn feed_scope(&mut self, n: usize) {
        let (l, r) = self.mixer.master(n);
        for k in 0..n {
            self.scope_phase += 1;
            if self.scope_phase >= SCOPE_DECIMATION {
                self.scope_phase = 0;
                // File pleine (interface fermée ou lente) : on jette, sans bloquer.
                let _ = self.scope.push(0.5 * (l[k] + r[k]));
            }
        }
    }

    fn drain_commands(&mut self) {
        for _ in 0..self.returning.len() {
            let g = self.returning.remove(0);
            self.trash(g);
        }
        while let Some(cmd) = self.commands.pop() {
            self.apply(cmd);
        }
    }

    fn silence_all(&mut self) {
        for ch in &mut self.channels {
            ch.silence();
        }
    }

    fn apply(&mut self, cmd: Command) {
        match cmd {
            Command::Play => {
                self.count_in = 0.0;
                self.playing = true;
                self.anchor();
            }
            Command::CountIn(ticks) => {
                if ticks == 0 {
                    self.count_in = 0.0;
                    self.playing = true;
                    self.anchor();
                } else {
                    self.playing = false;
                    self.count_in = ticks as f64;
                    self.count_len = ticks as f64;
                }
            }
            Command::Stop => {
                if !self.playing && self.count_in == 0.0 {
                    self.pos = 0.0;
                }
                self.count_in = 0.0;
                self.playing = false;
                self.silence_all();
                let now = self.clock;
                self.voice.silence(&mut self.channels, now);
                self.mixer.reset_effects();
                self.finish_recording(None);
            }
            Command::Locate(tick) => {
                self.pos = tick.max(0.0);
                self.anchor();
                self.silence_all();
            }
            Command::SetBpm(bpm) => {
                self.sequence.tempo.set_first_bpm(bpm);
                self.anchor();
            }
            Command::SetSequence(seq) => {
                // Dans la capacité réservée : pas d'allocation.
                self.voices.clear();
                let voices = seq.audio_clips.len().min(self.voices.capacity());
                self.voices.resize(voices, AudioVoice::default());
                let old = std::mem::replace(&mut self.sequence, seq);
                self.trash(Garbage::Sequence(old));
                // New tempo map (or mode): same position, counted from here.
                self.anchor();
            }
            Command::AddChannel(mut ch) => {
                if self.channels.len() < MAX_CHANNELS {
                    // Le canal sort de sa boîte ; la boîte et son bouche-trou repartent.
                    let mut channel = std::mem::replace(&mut *ch, Channel::new(String::new(), Box::new(Silence)));
                    channel.set_tempo(self.bpm as f32);
                    self.channels.push(channel);
                }
                self.trash(Garbage::Channel(ch));
            }
            Command::RemoveChannel(idx) => {
                if idx < self.channels.len() {
                    let now = self.clock;
                    self.voice.silence(&mut self.channels, now);
                    let mut ch = self.channels.remove(idx);
                    // Un instrument externe ne doit laisser aucune note pendue.
                    ch.silence();
                    drain_midi(&mut self.midi, &mut *ch.instrument, self.clock);
                    self.trash(Garbage::Channel(Box::new(ch)));
                }
            }
            Command::ReplaceChannel { channel, mut ch } => {
                if let Some(slot) = self.channels.get_mut(channel) {
                    slot.silence();
                    drain_midi(&mut self.midi, &mut *slot.instrument, self.clock);
                    std::mem::swap(slot, &mut *ch);
                    slot.set_tempo(self.bpm as f32);
                }
                self.trash(Garbage::Channel(ch));
            }
            Command::ReplaceInstrument { channel, instrument } => {
                if let Some(ch) = self.channels.get_mut(channel) {
                    ch.silence();
                    let mut old = std::mem::replace(&mut ch.instrument, instrument);
                    ch.instrument.set_tempo(self.bpm as f32);
                    drain_midi(&mut self.midi, &mut *old, self.clock);
                    self.trash(Garbage::Instrument(old));
                } else {
                    self.trash(Garbage::Instrument(instrument));
                }
            }
            Command::SetChannelGain { channel, gain } => {
                if let Some(ch) = self.channels.get_mut(channel) {
                    ch.gain = gain.max(0.0);
                }
            }
            Command::SetChannelPan { channel, pan } => {
                if let Some(ch) = self.channels.get_mut(channel) {
                    ch.pan = pan.clamp(-1.0, 1.0);
                }
            }
            Command::SetChannelMute { channel, muted } => {
                if let Some(ch) = self.channels.get_mut(channel) {
                    ch.muted = muted;
                    if muted {
                        ch.silence();
                    }
                }
            }
            Command::SetChannelPorta { channel, secs } => {
                if let Some(ch) = self.channels.get_mut(channel) {
                    ch.porta = secs.max(0.0);
                }
            }
            Command::SetChannelTrack { channel, track } => {
                if let Some(ch) = self.channels.get_mut(channel) {
                    ch.track = if track < self.mixer.len() { track } else { MASTER };
                }
            }
            Command::SetInstrumentParam { channel, id, value } => {
                if let Some(ch) = self.channels.get_mut(channel)
                    && !ch.mods.as_mut().is_some_and(|m| m.set_base(0, id, value))
                {
                    ch.instrument.set_param(id, value);
                }
            }
            Command::SetNoteFx { channel, slot, mut fx } => {
                if let Some(f) = fx.as_mut() {
                    f.set_tempo(self.bpm as f32);
                }
                if let Some(ch) = self.channels.get_mut(channel)
                    && slot < NOTE_FX_SLOTS
                {
                    ch.silence();
                    if let Some(old) = std::mem::replace(&mut ch.note_fx[slot], fx) {
                        self.trash(Garbage::NoteFx(old));
                    }
                } else if let Some(f) = fx {
                    self.trash(Garbage::NoteFx(f));
                }
            }
            Command::SetNoteFxParam { channel, slot, id, value } => {
                if let Some(Some(fx)) = self.channels.get_mut(channel).and_then(|ch| ch.note_fx.get_mut(slot)) {
                    fx.set_param(id, value);
                }
            }
            Command::NoteOn { channel, note, velocity } => {
                let now = self.clock;
                if let Some(ch) = self.channels.get_mut(channel) {
                    ch.note_in(now, crate::note_fx::NoteEvent::on(note, velocity));
                    ch.flush(now);
                }
            }
            Command::NoteOff { channel, note } => {
                let now = self.clock;
                if let Some(ch) = self.channels.get_mut(channel) {
                    ch.note_in(now, crate::note_fx::NoteEvent::off(note));
                    ch.flush(now);
                }
            }
            Command::SetTrackGain { track, gain } => {
                if let Some(t) = self.mixer.tracks.get_mut(track) {
                    t.gain = gain.max(0.0);
                }
            }
            Command::SetTrackPan { track, pan } => {
                if let Some(t) = self.mixer.tracks.get_mut(track) {
                    t.pan = pan.clamp(-1.0, 1.0);
                }
            }
            Command::SetTrackMute { track, muted } => {
                if let Some(t) = self.mixer.tracks.get_mut(track) {
                    t.muted = muted;
                }
            }
            Command::SetTrackOutput { track, output } => {
                let count = self.mixer.len();
                if let Some(t) = self.mixer.tracks.get_mut(track) {
                    t.output = if output < count { output } else { MASTER };
                }
            }
            Command::SetTrackVca { track, vca } => {
                let count = self.mixer.len();
                if let Some(t) = self.mixer.tracks.get_mut(track) {
                    t.vca = vca.filter(|&v| v < count && v != track);
                }
            }
            Command::SetTrackSend { track, slot, target, amount } => {
                let count = self.mixer.len();
                if let Some(t) = self.mixer.tracks.get_mut(track)
                    && slot < SEND_SLOTS
                {
                    t.sends[slot] = if target < count { (target, amount.max(0.0)) } else { (MASTER, 0.0) };
                }
            }
            Command::SetRoutingOrder(order) => {
                let old = std::mem::replace(&mut self.mixer.order, order);
                self.trash(Garbage::Routing(old));
            }
            Command::RemapTracks(remap) => self.remap_tracks(remap),
            Command::SetInsert { track, slot, mut effect } => {
                if let Some(fx) = effect.as_mut() {
                    fx.set_tempo(self.bpm as f32);
                }
                if let Some(t) = self.mixer.tracks.get_mut(track)
                    && slot < INSERT_SLOTS
                {
                    t.wake(slot);
                    if let Some(old) = std::mem::replace(&mut t.inserts[slot], effect) {
                        self.trash(Garbage::Effect(old));
                    }
                } else if let Some(fx) = effect {
                    self.trash(Garbage::Effect(fx));
                }
            }
            Command::SetInsertBypass { track, slot, bypass } => {
                if let Some(t) = self.mixer.tracks.get_mut(track)
                    && slot < INSERT_SLOTS
                {
                    t.bypass[slot] = bypass;
                }
            }
            Command::SetInsertSidechain { track, slot, key } => {
                let count = self.mixer.len();
                if let Some(t) = self.mixer.tracks.get_mut(track)
                    && slot < INSERT_SLOTS
                {
                    t.sidechain[slot] = key.filter(|&k| k < count && k != track);
                }
            }
            Command::SetEffectParam { track, slot, id, value } => {
                if let Some(t) = self.mixer.tracks.get_mut(track) {
                    t.wake(slot);
                }
                if let Some(t) = self.mixer.tracks.get_mut(track)
                    && !t.mods.as_mut().is_some_and(|m| m.set_base(slot, id, value))
                    && let Some(Some(fx)) = t.inserts.get_mut(slot)
                {
                    fx.set_param(id, value);
                }
            }
            Command::SetRackLayout { track, slot, layout } => {
                self.wake_insert(track, slot);
                if let Some(rack) = self.rack(track, slot) {
                    rack.set_layout(layout);
                }
            }
            Command::SetRackUnit { track, slot, unit, effect } => {
                self.wake_insert(track, slot);
                let rest = match self.rack(track, slot) {
                    Some(rack) => rack.set_unit(unit, effect),
                    None => effect,
                };
                if let Some(old) = rest {
                    self.trash(Garbage::Effect(old));
                }
            }
            Command::SetLayerDevice { channel, unit, device } => {
                use crate::instruments::inst_rack::LayerDevice;
                let rest = match self.channels.get_mut(channel).and_then(|c| c.instrument.as_inst_rack()) {
                    Some(rack) => rack.set_device(unit, device),
                    None => device,
                };
                match rest {
                    LayerDevice::None => {}
                    LayerDevice::Instrument(mut old) => {
                        drain_midi(&mut self.midi, &mut *old, self.clock);
                        self.trash(Garbage::Instrument(old));
                    }
                    LayerDevice::NoteFx(old) => self.trash(Garbage::NoteFx(old)),
                    LayerDevice::Effect(old) => self.trash(Garbage::Effect(old)),
                }
            }
            Command::SetLayerZone { channel, chain, zone } => {
                if let Some(rack) = self.channels.get_mut(channel).and_then(|c| c.instrument.as_inst_rack()) {
                    rack.set_zone(chain, zone);
                }
            }
            Command::SetLayerMapping { channel, index, map } => {
                if let Some(rack) = self.channels.get_mut(channel).and_then(|c| c.instrument.as_inst_rack()) {
                    rack.set_mapping(index, map);
                }
            }
            Command::SetRackMapping { track, slot, index, map } => {
                if let Some(rack) = self.rack(track, slot) {
                    rack.set_mapping(index, map);
                }
            }
            Command::SetTrackModulators { track, mut mods } => {
                if let Some(t) = self.mixer.tracks.get_mut(track) {
                    if let (Some(new), Some(old)) = (mods.as_mut(), t.mods.as_ref()) {
                        new.inherit(old);
                    }
                    mods = std::mem::replace(&mut t.mods, mods);
                }
                if let Some(old) = mods {
                    self.trash(Garbage::Modulators(old));
                }
            }
            Command::SetChannelModulators { channel, mut mods } => {
                if let Some(ch) = self.channels.get_mut(channel) {
                    if let (Some(new), Some(old)) = (mods.as_mut(), ch.mods.as_ref()) {
                        new.inherit(old);
                    }
                    mods = std::mem::replace(&mut ch.mods, mods);
                }
                if let Some(old) = mods {
                    self.trash(Garbage::Modulators(old));
                }
            }
            Command::SetChannelFrozen { channel, mut frozen } => {
                if let Some(ch) = self.channels.get_mut(channel) {
                    ch.silence();
                    frozen = std::mem::replace(&mut ch.frozen, frozen);
                }
                if let Some(old) = frozen {
                    self.trash(Garbage::Frozen(old));
                }
            }
            Command::SetTrackFrozen { track, mut frozen } => {
                if let Some(t) = self.mixer.tracks.get_mut(track).filter(|_| track != MASTER) {
                    frozen = std::mem::replace(&mut t.frozen, frozen);
                }
                if let Some(old) = frozen {
                    self.trash(Garbage::Frozen(old));
                }
            }
            Command::SetMetronome(on) => self.metronome = on,
            Command::HoldAutomation { target, hold } => {
                for h in &mut self.held {
                    if *h == Some(target) {
                        *h = None;
                    }
                }
                if hold && let Some(free) = self.held.iter_mut().find(|h| h.is_none()) {
                    *free = Some(target);
                }
            }
            Command::SetInput(input) => {
                if let Some(old) = std::mem::replace(&mut self.input, input) {
                    self.trash(Garbage::Input(old));
                }
            }
            Command::SetMonitor { track, source } => {
                if let Some(m) = self.mixer.monitor.get_mut(track) {
                    *m = source;
                }
            }
            Command::SetLowLatencyMonitoring(on) => self.low_latency = on,
            Command::SetSmartDisable(on) => self.set_smart_disable(on),
            Command::SetInputStrip(strip) => self.strip.set(strip),
            Command::SetVoice(settings) => {
                let now = self.clock;
                self.voice.set(settings, &mut self.channels, now);
            }
            Command::Record { track, armed, buffer } => {
                let track = track.min(self.mixer.len() - 1);
                self.finish_recording(Some(track));
                match buffer {
                    Some(mut rec) if armed => {
                        rec.len = 0;
                        rec.track = track;
                        rec.sample_rate = self.sample_rate;
                        rec.jumps.clear();
                        match self.recordings.iter_mut().find(|r| r.is_none()) {
                            Some(slot) => *slot = Some(rec),
                            // Plus de place : le tampon repart vide.
                            None => self.trash(Garbage::Recording(rec)),
                        }
                    }
                    Some(rec) => self.trash(Garbage::Recording(rec)),
                    None => {}
                }
                self.publish_record_room();
            }
            Command::RecordChunk { track, chunk } => {
                let rec = self.recordings.iter_mut().flatten().find(|r| r.track == track);
                match rec {
                    // Place des pointeurs préallouée : pas d'allocation ici.
                    Some(rec) if rec.chunks.len() < rec.chunks.capacity() && rec.is_mono() == chunk.is_mono() => {
                        rec.chunks.push(chunk);
                    }
                    _ => self.trash(Garbage::RecordChunk(chunk)),
                }
                self.publish_record_room();
            }
            Command::Audition(Some(mut voice)) => {
                // A loop previewed in time starts on the next beat of the song.
                if voice.wants_beat() && self.playing && self.count_in <= 0.0 {
                    let next = ((self.pos / PPQ as f64).floor() + 1.0) * PPQ as f64;
                    let wait = self.frames_to(next).clamp(0.0, 8.0 * self.sample_rate as f64);
                    voice.set_wait(wait as usize);
                }
                if let Some(old) = self.audition.replace(voice) {
                    self.trash(Garbage::Audition(old));
                }
            }
            Command::Audition(None) => {
                if let Some(voice) = self.audition.as_mut() {
                    voice.stop();
                }
            }
            Command::AuditionGain(gain) => {
                if let Some(voice) = self.audition.as_mut() {
                    voice.set_gain(gain);
                }
            }
            Command::Calibrate(buffer) => {
                if let Some(old) = self.calibration.take() {
                    self.trash(Garbage::Calibration(old));
                }
                if let Some(mut cal) = buffer {
                    cal.len = 0;
                    cal.sample_rate = self.sample_rate;
                    self.cal_clock = 0;
                    self.calibration = Some(cal);
                }
            }
            Command::Capture(buffer) => {
                if let Some(old) = self.capture.take() {
                    self.trash(Garbage::Capture(old));
                }
                if let Some(mut cap) = buffer {
                    cap.len = 0;
                    cap.sample_rate = self.sample_rate;
                    self.capture = Some(cap);
                }
            }
            Command::SetMidiOut(out) => {
                // L'horloge repart (Start ou Continue) sur la nouvelle sortie.
                let ports = self.midi_clock.ports;
                self.midi_clock.set_ports(&mut self.midi, 0, self.clock);
                self.midi_clock.ports = ports;
                if let Some(old) = std::mem::replace(&mut self.midi.out, out) {
                    self.trash(Garbage::MidiOut(old));
                }
                self.midi.begin(self.clock, self.mixer.latency());
            }
            Command::SetMidiClock { ports } => self.midi_clock.set_ports(&mut self.midi, ports, self.clock),
        }
        self.state.playing.store(self.playing, Ordering::Relaxed);
    }
}

/// Programme les messages MIDI que `instrument` a produits, à l'échantillon `now`.
fn drain_midi(link: &mut MidiLink, instrument: &mut dyn Instrument, now: u64) {
    instrument.drain_midi(&mut |ev| link.send(ev.port, now as i64 + ev.offset as i64, ev.message));
}

/// Largest delay the mixer of an engine at `sample_rate` compensates, in samples.
fn max_latency(sample_rate: f32) -> usize {
    (sample_rate.max(1.0) * MAX_LATENCY_SECONDS) as usize
}

/// Prepares a [`Command::RemapTracks`] for an engine at `sample_rate`:
/// `from[t]` is the current track that becomes track `t` (`None`: a new,
/// empty track), `order` the routing order of the new layout. Allocates: call
/// it off the audio thread.
pub fn track_remap(sample_rate: f32, from: &[Option<usize>], order: Box<[usize]>) -> Command {
    Command::RemapTracks(Box::new(TrackRemap::new(from, order, MAX_BLOCK, max_latency(sample_rate))))
}

/// A silent instrument: the placeholder of a frozen channel.
pub fn silent_instrument() -> Box<dyn Instrument> {
    Box::new(Silence)
}

/// Instrument muet, utilisé comme bouche-trou.
struct Silence;

impl crate::param::Params for Silence {}

impl Instrument for Silence {
    fn kind(&self) -> &'static str {
        "silence"
    }
    fn set_sample_rate(&mut self, _: f32) {}
    fn note_on(&mut self, _: u8, _: f32) {}
    fn note_off(&mut self, _: u8) {}
    fn all_notes_off(&mut self) {}
    fn render(&mut self, _: &mut [f32], _: &mut [f32]) {}
}

fn write_interleaved(out: &mut [f32], channels: usize, left: &[f32], right: &[f32]) {
    for (frame, (&l, &r)) in out.chunks_exact_mut(channels).zip(left.iter().zip(right)) {
        match frame {
            [mono] => *mono = 0.5 * (l + r),
            [fl, fr, rest @ ..] => {
                *fl = l;
                *fr = r;
                rest.fill(0.0);
            }
            [] => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mixer::DEFAULT_TRACKS;
    use crate::input::{CALIBRATION_SECONDS, InputProducer, input_channel, measure_round_trip, take_slices};
    use crate::sample::Sample;
    use crate::sequence::{AutoPoint, AutoScope, AutomationLane, Clip, Note, Pattern, TICKS_PER_STEP};
    use std::sync::Mutex;
    use std::sync::atomic::AtomicUsize;

    type LogEntry = (&'static str, u8, u64);

    /// Instrument espion : journalise (événement, note, échantillon).
    #[derive(Clone, Default)]
    struct Probe {
        log: Arc<Mutex<Vec<LogEntry>>>,
        rendered: Arc<AtomicUsize>,
        /// Follows tuning expressions.
        expressive: bool,
        /// (key, expression, sample).
        exprs: Arc<Mutex<Vec<(u8, crate::NoteExpression, u64)>>>,
        /// (controller, sample).
        ctls: Arc<Mutex<Vec<(crate::Controller, u64)>>>,
        /// (parameter, value, sample).
        params: Arc<Mutex<Vec<(u32, f32, u64)>>>,
    }

    impl crate::param::Params for Probe {
        fn set_param(&mut self, id: u32, value: f32) {
            let t = self.rendered.load(Ordering::Relaxed) as u64;
            self.params.lock().unwrap().push((id, value, t));
        }
    }

    impl Instrument for Probe {
        fn kind(&self) -> &'static str {
            "probe"
        }
        fn set_sample_rate(&mut self, _: f32) {}
        fn note_on(&mut self, note: u8, _: f32) {
            let t = self.rendered.load(Ordering::Relaxed) as u64;
            self.log.lock().unwrap().push(("on", note, t));
        }
        fn note_off(&mut self, note: u8) {
            let t = self.rendered.load(Ordering::Relaxed) as u64;
            self.log.lock().unwrap().push(("off", note, t));
        }
        fn all_notes_off(&mut self) {}
        fn supports_expression(&self) -> bool {
            self.expressive
        }
        fn note_expression(&mut self, key: u8, expr: crate::NoteExpression) {
            let t = self.rendered.load(Ordering::Relaxed) as u64;
            self.exprs.lock().unwrap().push((key, expr, t));
        }
        fn controller(&mut self, ctl: crate::Controller) {
            let t = self.rendered.load(Ordering::Relaxed) as u64;
            self.ctls.lock().unwrap().push((ctl, t));
        }
        fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
            self.rendered.fetch_add(left.len(), Ordering::Relaxed);
            left.fill(0.25);
            right.fill(0.25);
        }
    }

    const SR: f32 = 48_000.0;

    fn pattern(steps: &[usize]) -> Pattern {
        let mut p = Pattern::new(16 * TICKS_PER_STEP, 1);
        p.notes[0] = steps
            .iter()
            .map(|&s| Note { start: s as u32 * TICKS_PER_STEP, len: TICKS_PER_STEP, key: 60, vel: 1.0, ..Note::DEFAULT })
            .collect();
        p
    }

    fn setup(steps: &[usize]) -> (Engine, EngineHandle, Probe) {
        let probe = Probe::default();
        let ch = Channel::new("probe", Box::new(probe.clone())).with_track(1);
        let seq = Sequence { patterns: vec![pattern(steps)], ..Default::default() };
        let (engine, handle) = Engine::new(SR, vec![ch], seq);
        (engine, handle, probe)
    }

    fn run(engine: &mut Engine, frames: usize, block: usize) {
        let mut buf = vec![0.0; block * 2];
        let mut left = frames;
        while left > 0 {
            let n = left.min(block);
            engine.process(&mut buf[..n * 2], 2);
            left -= n;
        }
    }

    fn ons(probe: &Probe) -> Vec<u64> {
        probe.log.lock().unwrap().iter().filter(|e| e.0 == "on").map(|e| e.2).collect()
    }

    #[test]
    fn notes_fire_sample_accurately_regardless_of_block_size() {
        for block in [64, 333, 1024] {
            let (mut engine, mut handle, probe) = setup(&[0, 4, 8, 12]);
            handle.send(Command::SetBpm(120.0)).unwrap();
            handle.send(Command::Play).unwrap();
            // 120 BPM, 48 kHz : 250 échantillons par tick, 6000 par pas.
            run(&mut engine, 6_000 * 17, block);
            assert_eq!(ons(&probe), [0, 24_000, 48_000, 72_000, 96_000], "bloc {block}");
        }
    }

    fn expressive_setup(notes: Vec<Note>, porta: f32) -> (Engine, EngineHandle, Probe) {
        let probe = Probe { expressive: true, ..Default::default() };
        let ch = Channel::new("probe", Box::new(probe.clone())).with_track(1).with_porta(porta);
        let mut p = Pattern::new(4 * crate::TICKS_PER_BAR, 1);
        p.notes[0] = notes;
        p.sort();
        let (engine, mut handle) = Engine::new(SR, vec![ch], Sequence { patterns: vec![p], ..Default::default() });
        handle.send(Command::SetBpm(120.0)).unwrap();
        handle.send(Command::Play).unwrap();
        (engine, handle, probe)
    }

    #[test]
    fn slide_notes_glide_the_held_notes() {
        use crate::NoteExpression::{Pan, Tuning};
        for block in [64, 1000] {
            // 250 samples a tick. A held A3, a slide to A4 on its second beat
            // over half a beat, then a slide note right where a note ends (legato).
            let held = Note::new(0, 192, 57, 1.0);
            let slide = Note { slide: true, fine: 50.0, ..Note::new(96, 48, 69, 1.0) };
            let short = Note::new(384, 96, 60, 1.0);
            let legato = Note { slide: true, ..Note::new(480, 96, 62, 1.0) };
            let lone = Note { slide: true, pan: -0.5, ..Note::new(960, 24, 64, 1.0) };
            let (mut engine, _handle, probe) = expressive_setup(vec![held, slide, short, legato, lone], 0.0);
            run(&mut engine, 1_100 * 250, block);
            let log = probe.log.lock().unwrap().clone();
            // Slides play no note; the held notes last to the end of the slides;
            // a slide with nothing to glide plays as a note, with its pan.
            assert_eq!(
                log,
                [("on", 57, 0), ("off", 57, 48_000), ("on", 60, 96_000), ("off", 60, 144_000), ("on", 64, 240_000), ("off", 64, 246_000)],
                "block {block}"
            );
            let exprs = probe.exprs.lock().unwrap().clone();
            assert_eq!(
                exprs,
                [
                    (57, Tuning { semitones: 12.5, secs: 0.25 }, 24_000),
                    (60, Tuning { semitones: 2.0, secs: 0.5 }, 120_000),
                    (64, Pan(-0.5), 240_000),
                ],
                "block {block}"
            );
        }
    }

    #[test]
    fn controller_lanes_send_events_on_their_sample() {
        use crate::Controller::{Cc, PitchBend};
        let pt = |tick: u32, value: f32| AutoPoint { tick, value, curve: 0.0 };
        let scope = AutoScope::Pattern(0);
        let lanes = vec![
            AutomationLane { target: AutoTarget::PitchBend(0), scope, points: vec![pt(0, 0.0), pt(96, 1.0)] },
            AutomationLane { target: AutoTarget::Cc { channel: 0, cc: 1 }, scope, points: vec![pt(200, 0.0), pt(201, 0.5)] },
            AutomationLane { target: AutoTarget::InstrumentParam { channel: 0, id: 3 }, scope, points: vec![pt(192, 5.0)] },
            // Song lanes are not heard in pattern mode.
            AutomationLane { target: AutoTarget::PitchBend(0), scope: AutoScope::Song, points: vec![pt(0, -1.0)] },
        ];
        let mut runs = Vec::new();
        for block in [64, 1000] {
            let (mut engine, mut handle, probe) = setup(&[]);
            let seq = Sequence { patterns: vec![pattern(&[])], automation: lanes.clone(), ..Default::default() };
            handle.send(Command::SetSequence(Box::new(seq))).unwrap();
            handle.send(Command::SetBpm(120.0)).unwrap();
            handle.send(Command::Play).unwrap();
            // 250 samples a tick; the pattern loops after 384 ticks.
            run(&mut engine, 400 * 250, block);
            let ctls = probe.ctls.lock().unwrap().clone();
            let params = probe.params.lock().unwrap().clone();
            runs.push((ctls, params));
        }
        assert_eq!(runs[0], runs[1], "same events whatever the block size");
        let (ctls, params) = &runs[0];
        // The bend: its start value, then one value per tick of the ramp.
        let bends: Vec<(f32, u64)> = ctls.iter().filter_map(|&(c, t)| if let PitchBend(v) = c { Some((v, t)) } else { None }).collect();
        // Ticks 1 to 96, the loop at 384, then the ramp again up to 399.
        assert_eq!(bends.len(), 1 + 96 + 1 + 15, "{bends:?}");
        assert_eq!(bends[0], (0.0, 0));
        assert_eq!(bends[48], (0.5, 48 * 250));
        assert_eq!(bends[96], (1.0, 96 * 250));
        // The pattern loops: back to 0 on the loop's first sample.
        assert_eq!(bends[97], (0.0, 384 * 250));
        let ccs: Vec<(crate::Controller, u64)> = ctls.iter().copied().filter(|(c, _)| matches!(c, Cc(..))).collect();
        assert_eq!(ccs, [(Cc(1, 0.0), 0), (Cc(1, 0.5), 201 * 250), (Cc(1, 0.0), 384 * 250)]);
        assert_eq!(params, &[(3, 5.0, 0)]);
    }

    #[test]
    fn portamento_and_fine_pitch() {
        use crate::NoteExpression::Tuning;
        let a = Note { fine: -20.0, ..Note::new(0, 96, 60, 1.0) };
        let b = Note::new(96, 96, 67, 1.0);
        let (mut engine, _handle, probe) = expressive_setup(vec![a, b], 0.1);
        run(&mut engine, 200 * 250, 128);
        assert_eq!(
            probe.exprs.lock().unwrap().clone(),
            [
                (60, Tuning { semitones: -0.2, secs: 0.0 }, 0),
                (67, Tuning { semitones: 60.0 - 0.2 - 67.0, secs: 0.0 }, 24_000),
                (67, Tuning { semitones: 0.0, secs: 0.1 }, 24_000),
            ]
        );
    }

    #[test]
    fn notes_are_released_after_their_length() {
        let (mut engine, mut handle, probe) = setup(&[0]);
        handle.send(Command::Play).unwrap();
        run(&mut engine, 7_000, 512);
        assert_eq!(probe.log.lock().unwrap().clone(), [("on", 60, 0), ("off", 60, 6_000)]);
    }

    #[test]
    fn ratchets_and_conditions_are_sample_accurate() {
        for block in [64, 333, 1024] {
            let probe = Probe::default();
            let ch = Channel::new("probe", Box::new(probe.clone())).with_track(1);
            let mut p = Pattern::new(16 * TICKS_PER_STEP, 1);
            // Step 0: 4 repeats; step 8: one pass out of two.
            p.notes[0] = vec![
                Note { ratchet: 4, ..Note::new(0, TICKS_PER_STEP, 60, 1.0) },
                Note { cond: crate::NoteCond::Every(2, 2), ..Note::new(8 * TICKS_PER_STEP, TICKS_PER_STEP, 62, 1.0) },
            ];
            p.sort();
            let (mut engine, mut handle) = Engine::new(SR, vec![ch], Sequence { patterns: vec![p], ..Default::default() });
            handle.send(Command::SetBpm(120.0)).unwrap();
            handle.send(Command::Play).unwrap();
            // 6000 samples per step, 96 000 per pass: two passes.
            run(&mut engine, 192_000, block);
            let log = probe.log.lock().unwrap().clone();
            let on: Vec<(u8, u64)> = log.iter().filter(|e| e.0 == "on").map(|e| (e.1, e.2)).collect();
            let off: Vec<(u8, u64)> = log.iter().filter(|e| e.0 == "off").map(|e| (e.1, e.2)).collect();
            assert_eq!(
                on,
                [(60, 0), (60, 1_500), (60, 3_000), (60, 4_500), (60, 96_000), (60, 97_500), (60, 99_000), (60, 100_500), (62, 144_000)],
                "block {block}"
            );
            // Each repeat lasts a quarter of the step.
            assert_eq!(&off[..4], [(60, 1_500), (60, 3_000), (60, 4_500), (60, 6_000)], "block {block}");
        }
    }

    #[test]
    fn tempo_change_keeps_position() {
        let (mut engine, mut handle, probe) = setup(&[0, 1, 2]);
        handle.send(Command::Play).unwrap();
        run(&mut engine, 9_000, 100);
        handle.send(Command::SetBpm(240.0)).unwrap();
        run(&mut engine, 3_000, 100);
        // À 1,5 pas, le pas 2 tombe un demi-pas plus tard au nouveau tempo (1500 échantillons).
        assert_eq!(ons(&probe), [0, 6_000, 10_500]);
        assert!((handle.position() - (36.0 + 3_000.0 / 125.0)).abs() < 1e-6);
    }

    #[test]
    fn song_mode_loops_at_song_end() {
        let (mut engine, mut handle, probe) = setup(&[0]);
        let seq = Sequence {
            patterns: vec![pattern(&[0])],
            clips: vec![Clip { pattern: 0, start: 0, len: 96, offset: 0 }],
            mode: PlayMode::Song,
            song_len: 384,
            ..Default::default()
        };
        handle.send(Command::SetSequence(Box::new(seq))).unwrap();
        handle.send(Command::Play).unwrap();
        // Une mesure = 384 ticks = 96 000 échantillons ; on en joue deux.
        run(&mut engine, 192_000, 480);
        assert_eq!(ons(&probe), [0, 96_000]);
    }

    fn note_fx(kind: &str) -> Option<Box<dyn NoteFx>> {
        let mut fx = crate::registry::create_note_fx(kind).unwrap();
        fx.set_sample_rate(SR);
        Some(fx)
    }

    fn played(probe: &Probe, event: &str) -> Vec<(u8, u64)> {
        probe.log.lock().unwrap().iter().filter(|e| e.0 == event).map(|e| (e.1, e.2)).collect()
    }

    #[test]
    fn arpeggiator_is_sample_accurate_at_120_bpm() {
        for block in [64, 333, 1024] {
            let (mut engine, mut handle, probe) = setup(&[]);
            // Accord do-mi-sol tenu toute la mesure, joué dans le désordre.
            let mut p = Pattern::new(16 * TICKS_PER_STEP, 1);
            p.notes[0] =
                [67, 60, 64].map(|key| Note { start: 0, len: 16 * TICKS_PER_STEP, key, vel: 0.9, ..Note::DEFAULT }).to_vec();
            handle.send(Command::SetSequence(Box::new(Sequence { patterns: vec![p], ..Default::default() }))).unwrap();
            handle.send(Command::SetNoteFx { channel: 0, slot: 0, fx: note_fx("arpeggiator") }).unwrap();
            handle.send(Command::SetBpm(120.0)).unwrap();
            handle.send(Command::Play).unwrap();
            // Doubles croches à 120 BPM et 48 kHz : 6 000 échantillons, durée 50 %.
            run(&mut engine, 6_000 * 6, block);
            let keys = [60, 64, 67, 60, 64, 67];
            let want: Vec<(u8, u64)> = keys.iter().enumerate().map(|(i, &k)| (k, i as u64 * 6_000)).collect();
            assert_eq!(played(&probe, "on"), want, "bloc {block}");
            let want: Vec<(u8, u64)> = keys.iter().enumerate().map(|(i, &k)| (k, i as u64 * 6_000 + 3_000)).collect();
            assert_eq!(played(&probe, "off"), want, "bloc {block}");
        }
    }

    #[test]
    fn note_fx_chain_handles_live_notes_and_returns_garbage() {
        let (mut engine, mut handle, probe) = setup(&[]);
        handle.send(Command::SetNoteFx { channel: 0, slot: 1, fx: note_fx("chord") }).unwrap();
        // Gamme avant l'accord : do♯ devient do, puis do majeur.
        handle.send(Command::SetNoteFx { channel: 0, slot: 0, fx: note_fx("scale") }).unwrap();
        handle.send(Command::NoteOn { channel: 0, note: 61, velocity: 1.0 }).unwrap();
        run(&mut engine, 256, 256);
        handle.send(Command::NoteOff { channel: 0, note: 61 }).unwrap();
        run(&mut engine, 256, 256);
        assert_eq!(played(&probe, "on"), [(60, 0), (64, 0), (67, 0)]);
        assert_eq!(played(&probe, "off"), [(60, 256), (64, 256), (67, 256)]);

        handle.send(Command::SetNoteFxParam { channel: 0, slot: 1, id: 0, value: 3.0 }).unwrap();
        handle.send(Command::SetNoteFx { channel: 0, slot: 0, fx: None }).unwrap();
        handle.send(Command::NoteOn { channel: 0, note: 61, velocity: 1.0 }).unwrap();
        run(&mut engine, 256, 256);
        assert_eq!(played(&probe, "on")[3..], [(61, 512), (68, 512)]);
        assert!(matches!(handle.garbage.pop(), Some(Garbage::NoteFx(fx)) if fx.kind() == "scale"));
    }

    #[test]
    fn inserts_see_the_notes_a_channel_holds() {
        struct Watch(Arc<AtomicU64>);
        impl crate::param::Params for Watch {
            fn params(&self) -> &'static [crate::param::ParamInfo] {
                &[]
            }
            fn get_param(&self, _: u32) -> f32 {
                0.0
            }
            fn set_param(&mut self, _: u32, _: f32) {}
        }
        impl Effect for Watch {
            fn kind(&self) -> &'static str {
                "watch"
            }
            fn set_sample_rate(&mut self, _: f32) {}
            fn channel_notes(&mut self, notes: &[crate::ChannelNotes]) {
                let n = notes[0];
                self.0.store(n.held[0] | u64::from(n.last.unwrap_or(0)) << 56, Ordering::Relaxed);
            }
            fn process(&mut self, _: &mut [f32], _: &mut [f32]) {}
        }
        let (mut engine, mut handle, _) = setup(&[]);
        let seen = Arc::new(AtomicU64::new(0));
        handle.send(Command::SetInsert { track: 1, slot: 0, effect: Some(Box::new(Watch(seen.clone()))) }).unwrap();
        handle.send(Command::NoteOn { channel: 0, note: 48, velocity: 1.0 }).unwrap();
        handle.send(Command::NoteOn { channel: 0, note: 55, velocity: 1.0 }).unwrap();
        run(&mut engine, 256, 256);
        assert_eq!(seen.load(Ordering::Relaxed), (1 << 48 | 1 << 55) | 55 << 56);
        handle.send(Command::NoteOff { channel: 0, note: 55 }).unwrap();
        run(&mut engine, 256, 256);
        assert_eq!(seen.load(Ordering::Relaxed), 1 << 48 | 48 << 56);
    }

    #[test]
    fn mixer_routing_meters_and_garbage() {
        let (mut engine, mut handle, _) = setup(&[]);
        handle.send(Command::SetChannelGain { channel: 0, gain: 0.5 }).unwrap();
        handle.send(Command::SetTrackGain { track: 1, gain: 0.5 }).unwrap();
        let mut buf = vec![0.0; 512];
        engine.process(&mut buf, 2);
        assert_eq!(handle.take_channel_peak(0), [0.125, 0.125]);
        assert_eq!(handle.take_track_peak(1), [0.0625, 0.0625]);
        assert_eq!(handle.take_master_peak(), [0.0625, 0.0625]);
        assert_eq!(buf[0], 0.0625);
        assert!(handle.take_load() > 0.0);
        let mut n = 0;
        handle.drain_scope(|_| n += 1);
        assert_eq!(n, 128);

        handle.send(Command::SetSequence(Box::default())).unwrap();
        engine.process(&mut buf, 2);
        assert!(handle.garbage.pop().is_some(), "l'ancienne séquence revient à l'interface");
    }

    #[test]
    fn remapping_tracks_moves_their_state_and_renumbers_links() {
        let (mut engine, mut handle, _) = setup(&[]);
        // Channel 0 → track 1 (½) → track 3 (½) → master; a send to track 5, a VCA on 4.
        handle.send(Command::SetTrackGain { track: 1, gain: 0.5 }).unwrap();
        handle.send(Command::SetTrackOutput { track: 1, output: 3 }).unwrap();
        handle.send(Command::SetTrackGain { track: 3, gain: 0.5 }).unwrap();
        handle.send(Command::SetTrackSend { track: 1, slot: 0, target: 5, amount: 0.5 }).unwrap();
        handle.send(Command::SetTrackVca { track: 3, vca: Some(4) }).unwrap();
        let mut fx = crate::registry::create_effect("utility").unwrap();
        fx.set_sample_rate(SR);
        handle.send(Command::SetInsert { track: 3, slot: 2, effect: Some(fx) }).unwrap();
        handle.send(Command::SetInsertSidechain { track: 3, slot: 2, key: Some(5) }).unwrap();
        handle.send(Command::SetMonitor { track: 4, source: Some(InputSource::Left) }).unwrap();
        let mut edges = vec![vec![MASTER]; DEFAULT_TRACKS];
        edges[MASTER].clear();
        edges[1] = vec![3, 5];
        handle.send(Command::SetRoutingOrder(crate::mixer::routing_order(&edges).unwrap())).unwrap();
        run(&mut engine, 512, 256);
        handle.take_master_peak();
        run(&mut engine, 256, 256);
        let before = handle.take_master_peak();
        assert!((before[0] - 0.0625 - 0.125 * 0.5).abs() < 1e-6, "{before:?}");

        // A new track at 1 (the others move up by one), track 5 deleted, two appended.
        let mut from: Vec<Option<usize>> = vec![Some(MASTER), None];
        from.extend((1..DEFAULT_TRACKS).filter(|&t| t != 5).map(Some));
        from.extend([None, None]);
        let count = from.len();
        let mut edges = vec![vec![MASTER]; count];
        edges[MASTER].clear();
        edges[2] = vec![4];
        let order = crate::mixer::routing_order(&edges).unwrap();
        handle.send(track_remap(SR, &from, order)).unwrap();
        run(&mut engine, 256, 256);
        let m = &engine.mixer;
        assert_eq!(m.len(), DEFAULT_TRACKS + 2);
        assert_eq!(engine.channels[0].track, 2, "the channel follows its track");
        assert_eq!((m.tracks[2].gain, m.tracks[2].output), (0.5, 4));
        assert_eq!(m.tracks[2].sends[0], (MASTER, 0.0), "the send to the deleted track goes");
        assert_eq!(m.tracks[4].vca, Some(5));
        assert_eq!(m.tracks[4].inserts[2].as_ref().map(|fx| fx.kind()), Some("utility"));
        assert_eq!(m.tracks[4].sidechain[2], None, "the key on the deleted track goes");
        assert_eq!(m.monitor[5], Some(InputSource::Left));
        assert!(m.tracks[1].inserts.iter().all(Option::is_none) && m.tracks[1].gain == 1.0, "a new track is empty");
        handle.take_master_peak();
        run(&mut engine, 256, 256);
        assert_eq!(handle.take_master_peak()[0], 0.0625, "same path, the send gone");
        let old = std::iter::from_fn(|| handle.garbage.pop()).find_map(|g| match g {
            Garbage::Mixer(old) => Some(old),
            _ => None,
        });
        assert_eq!(old.expect("the old mixer comes back").len(), DEFAULT_TRACKS);
    }

    #[test]
    fn sidechain_compressor_follows_the_key_track() {
        // Canal 0 → piste 1 (compressée), canal 1 → piste 2 (clé) ; chacun sort 0,25.
        let measure = |key_muted: bool| {
            let (mut engine, mut handle, _) = setup(&[]);
            let key = Channel::new("clé", Box::new(Probe::default())).with_track(2);
            handle.send(Command::AddChannel(Box::new(key))).unwrap();
            handle.send(Command::SetChannelMute { channel: 1, muted: key_muted }).unwrap();
            let mut fx = crate::registry::create_effect("compressor").unwrap();
            fx.set_sample_rate(SR);
            fx.set_param(0, -30.0);
            fx.set_param(1, 20.0);
            handle.send(Command::SetInsert { track: 1, slot: 0, effect: Some(fx) }).unwrap();
            handle.send(Command::SetInsertSidechain { track: 1, slot: 0, key: Some(2) }).unwrap();
            let mut edges = vec![vec![MASTER]; DEFAULT_TRACKS];
            edges[MASTER].clear();
            edges[2].push(1);
            let order = crate::mixer::routing_order(&edges).unwrap();
            handle.send(Command::SetRoutingOrder(order)).unwrap();
            run(&mut engine, 24_000, 256);
            handle.take_track_peak(1);
            run(&mut engine, 256, 256);
            handle.take_track_peak(1)[0]
        };
        let loud = measure(false);
        let silent = measure(true);
        assert!(loud < 0.05, "clé jouée : {loud}");
        assert!((silent - 0.25).abs() < 0.03, "clé muette : {silent}");
    }

    #[test]
    fn master_latency_is_published() {
        let (mut engine, mut handle, _) = setup(&[]);
        run(&mut engine, 256, 256);
        assert_eq!(handle.latency(), 0);
        let mut fx = crate::registry::create_effect("limiter").unwrap();
        fx.set_sample_rate(SR);
        let lat = fx.latency();
        handle.send(Command::SetInsert { track: 1, slot: 0, effect: Some(fx) }).unwrap();
        run(&mut engine, 256, 256);
        assert_eq!(handle.latency(), lat);
        assert!((handle.latency_seconds() - lat as f32 / SR).abs() < 1e-9);
        handle.send(Command::SetInsertBypass { track: 1, slot: 0, bypass: true }).unwrap();
        run(&mut engine, 256, 256);
        assert_eq!(handle.latency(), 0);
    }

    #[test]
    fn channels_can_be_added_and_removed_live() {
        let (mut engine, mut handle, _) = setup(&[]);
        let extra = Probe::default();
        handle.send(Command::AddChannel(Box::new(Channel::new("b", Box::new(extra)).with_track(2)))).unwrap();
        let mut buf = vec![0.0; 64];
        engine.process(&mut buf, 2);
        assert_eq!(engine.channels.len(), 2);
        assert_eq!(handle.take_track_peak(2), [0.25, 0.25]);
        handle.send(Command::RemoveChannel(0)).unwrap();
        engine.process(&mut buf, 2);
        assert_eq!(engine.channels.len(), 1);
        assert_eq!(engine.channels[0].name, "b");
    }

    #[test]
    fn loop_range_wraps_playback() {
        let (mut engine, mut handle, probe) = setup(&[0, 1, 4]);
        let seq = Sequence { patterns: vec![pattern(&[0, 1, 4])], loop_range: Some((24, 72)), ..Default::default() };
        handle.send(Command::SetSequence(Box::new(seq))).unwrap();
        handle.send(Command::Locate(24.0)).unwrap();
        handle.send(Command::Play).unwrap();
        // Boucle de 48 ticks (12 000 échantillons) qui ne contient que le pas 1.
        run(&mut engine, 30_000, 256);
        assert_eq!(ons(&probe), [0, 12_000, 24_000]);
    }

    /// A selection running past the pattern's end hears silence there, not bar 1 again.
    #[test]
    fn loop_past_the_pattern_end_stays_silent() {
        let (mut engine, mut handle, probe) = setup(&[0, 4]);
        let seq = Sequence { patterns: vec![pattern(&[0, 4])], loop_range: Some((12 * TICKS_PER_STEP, 28 * TICKS_PER_STEP)), ..Default::default() };
        handle.send(Command::SetSequence(Box::new(seq))).unwrap();
        handle.send(Command::Locate((12 * TICKS_PER_STEP) as f64)).unwrap();
        handle.send(Command::Play).unwrap();
        run(&mut engine, 200_000, 256);
        assert!(ons(&probe).is_empty(), "{:?}", ons(&probe));
    }

    /// Canal External MIDI (port 1, canal MIDI 3) sur un pattern, la file de
    /// sortie MIDI servant de port factice.
    fn setup_midi(steps: &[usize]) -> (Engine, EngineHandle, ponzik_midi::out::MessageReceiver) {
        let mut ext = crate::registry::create_instrument("midi_out").unwrap();
        ext.set_param(0, 1.0);
        ext.set_param(1, 2.0);
        let ch = Channel::new("synth", ext);
        let seq = Sequence { patterns: vec![pattern(steps)], ..Default::default() };
        let (engine, mut handle) = Engine::new(SR, vec![ch], seq);
        let (tx, rx) = ponzik_midi::out::queue(16_384);
        handle.send(Command::SetMidiOut(Some(Box::new(MidiOut::new(tx, 0.0))))).unwrap();
        (engine, handle, rx)
    }

    fn midi_sent(rx: &mut ponzik_midi::out::MessageReceiver) -> Vec<(u8, u64, Vec<u8>)> {
        std::iter::from_fn(|| rx.pop()).map(|m| (m.port, m.frame, m.message.bytes().to_vec())).collect()
    }

    #[test]
    fn external_midi_notes_and_clock_are_sample_accurate() {
        for block in [64, 333, 1024] {
            let (mut engine, mut handle, mut rx) = setup_midi(&[0, 4]);
            handle.send(Command::SetMidiClock { ports: 0b1 }).unwrap();
            handle.send(Command::Play).unwrap();
            // 120 BPM, 48 kHz : 250 échantillons par tick, 1 000 par impulsion, 6 000 par pas.
            run(&mut engine, 30_000, block);
            let sent = midi_sent(&mut rx);
            let notes: Vec<(u8, u64, Vec<u8>)> = sent.iter().filter(|m| m.0 == 1).cloned().collect();
            assert_eq!(
                notes,
                [
                    (1, 0, vec![0x92, 60, 127]),
                    (1, 6_000, vec![0x82, 60, 0]),
                    (1, 24_000, vec![0x92, 60, 127]),
                ],
                "block {block}"
            );
            let clock: Vec<(u64, Vec<u8>)> = sent.iter().filter(|m| m.0 == 0).map(|m| (m.1, m.2.clone())).collect();
            assert_eq!(clock[0], (0, vec![0xFA]), "block {block}");
            let pulses: Vec<u64> = clock[1..].iter().map(|c| c.0).collect();
            assert_eq!(pulses, (0..30).map(|k| k * 1_000).collect::<Vec<_>>(), "block {block}");
            assert!(clock[1..].iter().all(|c| c.1 == [0xF8]));
            // Ordre d'émission : l'horloge d'un instant passe avant la note du même instant.
            let first_note = sent.iter().position(|m| m.0 == 1).unwrap();
            assert_eq!(sent[..first_note].len(), 2);
        }
    }

    #[test]
    fn stop_releases_external_notes_and_stops_the_clock() {
        let (mut engine, mut handle, mut rx) = setup_midi(&[0]);
        handle.send(Command::SetMidiClock { ports: 0b1 }).unwrap();
        handle.send(Command::Play).unwrap();
        run(&mut engine, 3_000, 256);
        midi_sent(&mut rx);
        handle.send(Command::Stop).unwrap();
        run(&mut engine, 512, 256);
        let sent = midi_sent(&mut rx);
        let at = 3_000; // Début du bloc qui suit l'arrêt.
        assert!(sent.contains(&(1, at, vec![0x82, 60, 0])), "{sent:?}");
        assert!(sent.contains(&(1, at, vec![0xB2, 123, 0])), "{sent:?}");
        assert!(sent.contains(&(0, at, vec![0xFC])), "{sent:?}");
        assert!(!sent.iter().any(|m| m.2 == [0xF8]), "no clock once stopped");
        // Locate à l'arrêt : Song Position Pointer (mesure 2 = 16 doubles croches).
        handle.send(Command::Locate(384.0)).unwrap();
        run(&mut engine, 256, 256);
        assert_eq!(midi_sent(&mut rx), [(0, 3_512, vec![0xF2, 16, 0])]);
    }

    #[test]
    fn removing_an_external_channel_releases_its_notes() {
        let (mut engine, mut handle, mut rx) = setup_midi(&[0]);
        handle.send(Command::Play).unwrap();
        run(&mut engine, 1_000, 500);
        midi_sent(&mut rx);
        handle.send(Command::RemoveChannel(0)).unwrap();
        run(&mut engine, 500, 500);
        let sent: Vec<Vec<u8>> = midi_sent(&mut rx).into_iter().map(|m| m.2).collect();
        assert_eq!(sent, [vec![0x82, 60, 0], vec![0xB2, 123, 0]]);
    }

    #[test]
    fn stop_twice_returns_to_start() {
        let (mut engine, mut handle, _) = setup(&[]);
        handle.send(Command::Play).unwrap();
        run(&mut engine, 4_800, 480);
        handle.send(Command::Stop).unwrap();
        run(&mut engine, 480, 480);
        assert!(handle.position() > 0.0);
        handle.send(Command::Stop).unwrap();
        run(&mut engine, 480, 480);
        assert_eq!(handle.position(), 0.0);
    }

    #[test]
    fn metronome_clicks_on_beats() {
        let (mut engine, mut handle, _) = setup(&[]);
        handle.send(Command::SetChannelMute { channel: 0, muted: true }).unwrap();
        handle.send(Command::SetMetronome(true)).unwrap();
        handle.send(Command::Play).unwrap();
        let mut buf = vec![0.0; 2 * 2_000];
        engine.process(&mut buf, 2);
        assert!(buf.iter().any(|s| s.abs() > 0.05), "clic au premier temps");
    }

    #[test]
    fn count_in_clicks_one_bar_then_plays() {
        for block in [100, 512] {
            let (mut engine, mut handle, probe) = setup(&[0]);
            handle.send(Command::SetChannelMute { channel: 0, muted: true }).unwrap();
            handle.send(Command::CountIn(4 * PPQ)).unwrap();
            // Une mesure à 120 BPM : 96 000 échantillons de clics, sans note ni avancée.
            let mut buf = vec![0.0; 2 * 96_000];
            for chunk in buf.chunks_mut(2 * block) {
                engine.process(chunk, 2);
            }
            // Pré-compte fini pile à la fin du bloc : la lecture est lancée, toujours au début.
            assert_eq!(handle.count_in(), 0.0);
            assert!(handle.is_playing());
            assert_eq!(handle.position(), 0.0);
            assert!(ons(&probe).is_empty(), "bloc {block}");
            // Quatre clics, un par temps, le premier accentué (même métronome coupé).
            for beat in 0..4 {
                let at = beat * 24_000;
                assert!(buf[2 * at..2 * (at + 200)].iter().any(|s| s.abs() > 0.05), "temps {beat}");
                assert!(buf[2 * (at + 12_000)..2 * (at + 12_100)].iter().all(|s| s.abs() < 1e-3));
            }
            // Puis la lecture part du début : la première note tombe juste après.
            handle.send(Command::SetChannelMute { channel: 0, muted: false }).unwrap();
            run(&mut engine, 1_000, block);
            assert_eq!(ons(&probe), [96_000], "bloc {block}");
            assert!((handle.position() - 1_000.0 / 250.0).abs() < 1e-6);
        }
    }

    #[test]
    fn stop_cancels_count_in_without_rewinding() {
        let (mut engine, mut handle, probe) = setup(&[0]);
        handle.send(Command::Locate(96.0)).unwrap();
        handle.send(Command::CountIn(4 * PPQ)).unwrap();
        run(&mut engine, 10_000, 256);
        assert!(handle.count_in() > 300.0);
        handle.send(Command::Stop).unwrap();
        run(&mut engine, 200_000, 256);
        assert_eq!(handle.count_in(), 0.0);
        assert!(!handle.is_playing());
        assert_eq!(handle.position(), 96.0);
        assert!(ons(&probe).is_empty());
    }

    fn lane(target: AutoTarget, scope: AutoScope, points: &[(u32, f32)]) -> AutomationLane {
        AutomationLane {
            target,
            scope,
            points: points.iter().map(|&(tick, value)| AutoPoint { tick, value, curve: 0.0 }).collect(),
        }
    }

    /// Crête de la piste sur les `256` échantillons qui suivent l'échantillon `at`.
    fn track_peak_at(engine: &mut Engine, handle: &EngineHandle, done: &mut usize, at: usize, track: usize) -> f32 {
        run(engine, at - *done, 256);
        handle.take_track_peak(track);
        run(engine, 256, 256);
        *done = at + 256;
        handle.take_track_peak(track)[0]
    }

    #[test]
    fn track_gain_lane_is_applied() {
        let (mut engine, mut handle, _) = setup(&[]);
        let seq = Sequence {
            mode: PlayMode::Song,
            song_len: 4 * 384,
            automation: vec![lane(AutoTarget::TrackGain(1), AutoScope::Song, &[(96, 1.0), (192, 0.2)])],
            ..Default::default()
        };
        handle.send(Command::SetSequence(Box::new(seq))).unwrap();
        handle.send(Command::Play).unwrap();
        // 250 échantillons par tick ; le canal sonde sort 0,25 en continu.
        let mut done = 0;
        assert!((track_peak_at(&mut engine, &handle, &mut done, 0, 1) - 0.25).abs() < 1e-6);
        assert!((track_peak_at(&mut engine, &handle, &mut done, 250 * 300, 1) - 0.05).abs() < 1e-6);
        // À l'arrêt, la dernière valeur reste.
        handle.send(Command::Stop).unwrap();
        handle.send(Command::Locate(0.0)).unwrap();
        run(&mut engine, 512, 256);
        handle.take_track_peak(1);
        run(&mut engine, 256, 256);
        assert!((handle.take_track_peak(1)[0] - 0.05).abs() < 1e-6);
    }

    #[test]
    fn held_lanes_leave_the_parameter_alone() {
        let (mut engine, mut handle, _) = setup(&[]);
        let seq = Sequence {
            mode: PlayMode::Song,
            song_len: 4 * 384,
            automation: vec![lane(AutoTarget::TrackGain(1), AutoScope::Song, &[(0, 0.2)])],
            ..Default::default()
        };
        handle.send(Command::SetSequence(Box::new(seq))).unwrap();
        handle.send(Command::HoldAutomation { target: AutoTarget::TrackGain(1), hold: true }).unwrap();
        handle.send(Command::SetTrackGain { track: 1, gain: 1.0 }).unwrap();
        handle.send(Command::Play).unwrap();
        let mut done = 0;
        assert!((track_peak_at(&mut engine, &handle, &mut done, 2_000, 1) - 0.25).abs() < 1e-6);
        handle.send(Command::HoldAutomation { target: AutoTarget::TrackGain(1), hold: false }).unwrap();
        assert!((track_peak_at(&mut engine, &handle, &mut done, 6_000, 1) - 0.05).abs() < 1e-6);
    }

    #[test]
    fn pattern_automation_loops_with_the_pattern() {
        let (mut engine, mut handle, _) = setup(&[]);
        // Pattern d'une mesure (384 ticks) : plein volume puis silence à mi-mesure.
        let seq = Sequence {
            patterns: vec![pattern(&[])],
            automation: vec![lane(
                AutoTarget::TrackGain(1),
                AutoScope::Pattern(0),
                &[(0, 1.0), (180, 1.0), (192, 0.0)],
            )],
            ..Default::default()
        };
        handle.send(Command::SetSequence(Box::new(seq))).unwrap();
        handle.send(Command::Play).unwrap();
        let mut done = 0;
        for bar in 0..3 {
            let base = bar * 384 * 250;
            let loud = track_peak_at(&mut engine, &handle, &mut done, base + 250 * 100, 1);
            let quiet = track_peak_at(&mut engine, &handle, &mut done, base + 250 * 300, 1);
            assert!((loud - 0.25).abs() < 1e-6, "mesure {bar} : {loud}");
            assert_eq!(quiet, 0.0, "mesure {bar}");
        }
    }

    #[test]
    fn tempo_map_ramp_places_notes_on_the_exact_sample() {
        use crate::tempo_map::{TempoMap, TempoPoint};
        let map = TempoMap::new(
            vec![
                TempoPoint { tick: 0, bpm: 120.0, ramp: false },
                TempoPoint { tick: 96, bpm: 120.0, ramp: false },
                TempoPoint { tick: 288, bpm: 240.0, ramp: true },
                TempoPoint { tick: 336, bpm: 90.0, ramp: false },
            ],
            Vec::new(),
        );
        let steps: Vec<usize> = (0..16).collect();
        let expected: Vec<u64> = (0..16)
            .map(|s| ((map.secs_at((s * TICKS_PER_STEP) as f64) * SR as f64) - 1e-6).ceil() as u64)
            .collect();
        for block in [64, 333, 1024] {
            let (mut engine, mut handle, probe) = setup(&steps);
            let seq = Sequence {
                patterns: vec![pattern(&steps)],
                clips: vec![Clip { pattern: 0, start: 0, len: 384, offset: 0 }],
                mode: PlayMode::Song,
                song_len: 384 * 4,
                tempo: map.clone(),
                ..Default::default()
            };
            handle.send(Command::SetSequence(Box::new(seq))).unwrap();
            handle.send(Command::Play).unwrap();
            let frames = 100_000;
            run(&mut engine, frames, block);
            assert_eq!(ons(&probe), expected, "block {block}");
            let want = map.tick_at_secs(frames as f64 / SR as f64);
            assert!((handle.position() - want).abs() < 1e-6, "block {block}");
        }
    }

    /// Click onsets (frame, accented) of a render: a click starts after silence;
    /// the accent is the higher pitch, counted by zero crossings over 10 ms.
    fn clicks(buf: &[f32]) -> Vec<(usize, bool)> {
        let left: Vec<f32> = buf.iter().step_by(2).copied().collect();
        let mut out = Vec::new();
        let mut quiet = usize::MAX;
        for (i, &s) in left.iter().enumerate() {
            if s.abs() > 1e-3 {
                if quiet > 2_000 {
                    let w = &left[i..(i + 480).min(left.len())];
                    let crossings = w.windows(2).filter(|p| (p[0] >= 0.0) != (p[1] >= 0.0)).count();
                    out.push((i, crossings > 30));
                }
                quiet = 0;
            } else {
                quiet = quiet.saturating_add(1);
            }
        }
        out
    }

    #[test]
    fn metronome_accents_follow_the_time_signature_map() {
        use crate::tempo_map::{TempoMap, TimeSig};
        let (mut engine, mut handle, _) = setup(&[]);
        let tempo = TempoMap::new(Vec::new(), vec![TimeSig { tick: 0, num: 3, den: 4 }, TimeSig { tick: 576, num: 6, den: 8 }]);
        let seq = Sequence { mode: PlayMode::Song, song_len: 384 * 8, tempo, ..Default::default() };
        handle.send(Command::SetSequence(Box::new(seq))).unwrap();
        handle.send(Command::SetChannelMute { channel: 0, muted: true }).unwrap();
        handle.send(Command::SetMetronome(true)).unwrap();
        handle.send(Command::Play).unwrap();
        let mut buf = vec![0.0; 2 * 250 * 870];
        for chunk in buf.chunks_mut(2 * 300) {
            engine.process(chunk, 2);
        }
        // 3/4 in quarters, then 6/8 in eighths; 250 samples per tick.
        let beats = [0, 96, 192, 288, 384, 480, 576, 624, 672, 720, 768, 816, 864];
        let accents = [0, 288, 576, 864];
        let want: Vec<(usize, bool)> = beats.iter().map(|&t| (t * 250, accents.contains(&t))).collect();
        assert_eq!(clicks(&buf), want);
    }

    #[test]
    fn count_in_counts_the_bar_of_the_signature() {
        use crate::tempo_map::{TempoMap, TimeSig};
        let (mut engine, mut handle, probe) = setup(&[0]);
        let tempo = TempoMap::new(Vec::new(), vec![TimeSig { tick: 0, num: 3, den: 4 }]);
        let seq = Sequence { patterns: vec![pattern(&[0])], tempo, ..Default::default() };
        handle.send(Command::SetSequence(Box::new(seq))).unwrap();
        handle.send(Command::SetChannelMute { channel: 0, muted: true }).unwrap();
        handle.send(Command::CountIn(288)).unwrap();
        let mut buf = vec![0.0; 2 * 72_000];
        for chunk in buf.chunks_mut(2 * 256) {
            engine.process(chunk, 2);
        }
        assert_eq!(clicks(&buf), [(0, true), (24_000, false), (48_000, false)]);
        assert!(handle.is_playing());
        handle.send(Command::SetChannelMute { channel: 0, muted: false }).unwrap();
        run(&mut engine, 1_000, 256);
        assert_eq!(ons(&probe), [72_000]);
    }

    #[test]
    fn audio_clip_starts_at_its_tick_through_the_tempo_map() {
        use crate::tempo_map::{TempoMap, TempoPoint};
        let left = vec![0.5f32; 4_800];
        let sample = Sample { name: "dc".into(), sample_rate: SR, right: left.clone(), left, markers: Vec::new() };
        let (mut engine, mut handle, _) = setup(&[]);
        // 96 ticks at 120 BPM (0.5 s), then 60 BPM: tick 192 is at 1.5 s.
        let tempo = TempoMap::new(vec![TempoPoint { tick: 0, bpm: 120.0, ramp: false }, TempoPoint { tick: 96, bpm: 60.0, ramp: false }], Vec::new());
        let seq = Sequence {
            mode: PlayMode::Song,
            song_len: 384 * 8,
            audio_clips: vec![audio_clip(sample, 192, 0.0, 1.0)],
            tempo,
            ..Default::default()
        };
        handle.send(Command::SetChannelMute { channel: 0, muted: true }).unwrap();
        handle.send(Command::SetSequence(Box::new(seq))).unwrap();
        handle.send(Command::Play).unwrap();
        let mut buf = vec![0.0; 2 * 80_000];
        for chunk in buf.chunks_mut(2 * 500) {
            engine.process(chunk, 2);
        }
        let first = buf.iter().step_by(2).position(|&s| s != 0.0);
        assert_eq!(first, Some(72_000));
    }

    fn audio_clip(sample: Sample, start: u32, offset: f64, gain: f32) -> AudioClip {
        AudioClip { sample: Arc::new(sample), track: 2, start, len: 384 * 4, offset, gain, ..Default::default() }
    }

    /// Moteur en mode morceau avec ces clips audio, canal sonde coupé ; rend `frames` (gauche seulement).
    fn render_audio(clips: Vec<AudioClip>, frames: usize, block: usize) -> Vec<f32> {
        render_audio_in(PlayMode::Song, clips, frames, block)
    }

    fn render_audio_in(mode: PlayMode, clips: Vec<AudioClip>, frames: usize, block: usize) -> Vec<f32> {
        let (mut engine, mut handle, _) = setup(&[]);
        let seq = Sequence { mode, song_len: 384 * 8, audio_clips: clips, ..Default::default() };
        handle.send(Command::SetChannelMute { channel: 0, muted: true }).unwrap();
        handle.send(Command::SetSequence(Box::new(seq))).unwrap();
        handle.send(Command::Play).unwrap();
        let mut out = Vec::with_capacity(frames);
        let mut buf = vec![0.0; 2 * block];
        while out.len() < frames {
            let n = (frames - out.len()).min(block);
            engine.process(&mut buf[..2 * n], 2);
            out.extend(buf[..2 * n].iter().step_by(2));
        }
        out
    }

    #[test]
    fn audio_clip_starts_on_the_sample_with_offset_and_gain() {
        // Rampe : la valeur dit quelle image de la source est jouée.
        let n = 4_800;
        let left: Vec<f32> = (0..n).map(|i| i as f32 / n as f32).collect();
        let sample = Sample { name: "rampe".into(), sample_rate: SR, right: left.clone(), left, markers: Vec::new() };
        for block in [64, 300, 1024] {
            // Début au tick 96 = échantillon 24 000 ; décalage 10 ms = 480 images.
            let out = render_audio(vec![audio_clip(sample.clone(), 96, 0.01, 0.5)], 30_000, block);
            assert!(out[..24_000].iter().all(|&s| s == 0.0), "bloc {block}");
            assert!(out[24_000] > 0.0, "bloc {block}");
            // Après le fondu de 2 ms (96 échantillons).
            for k in [200, 1_000, 3_000] {
                let want = 0.5 * (480 + k) as f32 / n as f32;
                assert!((out[24_000 + k] - want).abs() < 1e-5, "bloc {block}, {k} : {} ≠ {want}", out[24_000 + k]);
            }
            // L'échantillon s'arrête à sa fin : 4 800 − 480 images.
            assert!(out[24_000 + 4_319] > 0.0);
            assert!(out[24_000 + 4_320..].iter().all(|&s| s == 0.0), "bloc {block}");
        }
    }

    #[test]
    fn session_audio_clip_cycles_from_its_phase() {
        let n = 48_000;
        let left: Vec<f32> = (0..n).map(|i| i as f32 / n as f32).collect();
        let sample = Sample { name: "rampe".into(), sample_rate: SR, right: left.clone(), left, markers: Vec::new() };
        // From tick 96 (sample 24 000), a cycle of 48 ticks (12 000 samples)
        // entered 24 ticks in (legato), until tick 288.
        let mut clip = audio_clip(sample, 96, 0.0, 1.0);
        (clip.len, clip.cycle, clip.phase) = (192, 48, 24);
        for block in [64, 333, 1024] {
            let out = render_audio_in(PlayMode::Session, vec![clip.clone()], 80_000, block);
            assert!(out[..24_000].iter().all(|&s| s == 0.0), "block {block}");
            for k in [500, 3_000, 5_500, 6_500, 12_000, 30_000, 47_000] {
                let want = ((k + 6_000) % 12_000) as f32 / n as f32;
                assert!((out[24_000 + k] - want).abs() < 1e-5, "block {block}, {k}: {} ≠ {want}", out[24_000 + k]);
            }
            // Faded out before each restart, silent after the end.
            assert!(out[24_000 + 5_999].abs() < 0.01, "block {block}: {}", out[24_000 + 5_999]);
            assert!(out[72_000..].iter().all(|&s| s == 0.0), "block {block}");
        }
        // Pattern mode plays no audio clip.
        let out = render_audio_in(PlayMode::Pattern(0), vec![clip], 40_000, 256);
        assert!(out.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn audio_clip_is_resampled_to_the_engine_rate() {
        let sample =
            Sample { name: "dc".into(), sample_rate: 24_000.0, left: vec![1.0; 1_000], right: vec![1.0; 1_000], markers: Vec::new() };
        let out = render_audio(vec![audio_clip(sample, 0, 0.0, 1.0)], 4_000, 256);
        // 1 000 images à 24 kHz = 2 000 échantillons à 48 kHz.
        let audible: Vec<usize> = (0..out.len()).filter(|&i| out[i] != 0.0).collect();
        assert_eq!(audible.len(), 2_000);
        assert_eq!((audible[0], audible[1_999]), (0, 1_999));
        assert!((out[1_000] - 1.0).abs() < 1e-6);
        assert!(out[0] < 0.05 && out[1_999] < 0.05, "fondus aux bords");
    }

    #[test]
    fn audio_clip_follows_locate_and_clip_end() {
        let sample = Sample { name: "dc".into(), sample_rate: SR, left: vec![1.0; 96_000], right: vec![1.0; 96_000], markers: Vec::new() };
        let mut clip = audio_clip(sample, 0, 0.0, 1.0);
        clip.len = 96;
        let (mut engine, mut handle, _) = setup(&[]);
        let seq = Sequence { mode: PlayMode::Song, song_len: 384, audio_clips: vec![clip], ..Default::default() };
        handle.send(Command::SetChannelMute { channel: 0, muted: true }).unwrap();
        handle.send(Command::SetSequence(Box::new(seq))).unwrap();
        handle.send(Command::Play).unwrap();
        let mut buf = vec![0.0; 2 * 30_000];
        engine.process(&mut buf, 2);
        // Le clip dure 96 ticks = 24 000 échantillons, fondu de sortie compris.
        assert!(buf[2 * 23_999] > 0.0 && buf[2 * 23_999] < 0.05);
        assert!(buf[2 * 24_000..].iter().all(|&s| s == 0.0));
        assert!((buf[2 * 12_000] - 1.0).abs() < 1e-6);
        // Saut au milieu du clip : reprise immédiate avec un fondu court.
        handle.send(Command::Locate(48.0)).unwrap();
        engine.process(&mut buf[..2 * 512], 2);
        assert!(buf[0] > 0.0 && buf[0] < 0.05);
        assert!((buf[2 * 200] - 1.0).abs() < 1e-6);
        // En mode pattern, les clips audio se taisent.
        let silent = Sequence { patterns: vec![pattern(&[])], ..Default::default() };
        handle.send(Command::SetSequence(Box::new(silent))).unwrap();
        engine.process(&mut buf[..2 * 512], 2);
        assert!(buf[..2 * 512].iter().all(|&s| s == 0.0));
    }

    #[test]
    fn warped_clip_follows_the_tempo_map() {
        use crate::sequence::{ClipWarp, WarpMap};
        // A ramp at 120 BPM in the file: the value says which beat is playing.
        let n = 48_000;
        let left: Vec<f32> = (0..n).map(|i| i as f32 / n as f32).collect();
        let sample = Sample { name: "rampe".into(), sample_rate: SR, right: left.clone(), left, markers: Vec::new() };
        let mut clip = audio_clip(sample, 0, 0.0, 1.0);
        // The project runs at 120 BPM by default: a 60 BPM file plays twice as fast.
        clip.warp = Some(ClipWarp { map: WarpMap::new(60.0, Vec::new()), rendered: None });
        for block in [64, 500] {
            let out = render_audio(vec![clip.clone()], 30_000, block);
            for k in [1_000usize, 10_000, 20_000] {
                let want = (2 * k) as f32 / n as f32;
                assert!((out[k] - want).abs() < 1e-4, "block {block}, {k}: {} ≠ {want}", out[k]);
            }
            // The file ends at half its duration.
            assert!(out[24_100..].iter().all(|&s| s == 0.0), "block {block}");
        }
    }

    #[test]
    fn warped_clip_plays_its_rendered_stretch() {
        use crate::sequence::{ClipWarp, WarpMap};
        let src = Sample { name: "dc".into(), sample_rate: SR, left: vec![0.25; 9_600], right: vec![0.25; 9_600], markers: Vec::new() };
        let stretched: Vec<f32> = (0..20_000).map(|i| i as f32 / 20_000.0).collect();
        let rendered = Sample { name: String::new(), sample_rate: SR, right: stretched.clone(), left: stretched, markers: Vec::new() };
        let mut clip = audio_clip(src, 96, 0.0, 1.0);
        clip.warp = Some(ClipWarp { map: WarpMap::new(120.0, Vec::new()), rendered: Some(Arc::new(rendered)) });
        let out = render_audio(vec![clip], 50_000, 256);
        assert!(out[..24_000].iter().all(|&s| s == 0.0));
        for k in [500, 5_000, 19_000] {
            assert!((out[24_000 + k] - k as f32 / 20_000.0).abs() < 1e-5, "{k}");
        }
        assert!(out[44_000..].iter().all(|&s| s == 0.0));
    }

    #[test]
    fn equal_power_fades_sum_to_unit_power() {
        for i in 0..=100 {
            let x = i as f64 / 100.0;
            let (a, b) = (equal_power(x), equal_power(1.0 - x));
            assert!((a * a + b * b - 1.0).abs() < 1e-12, "{x}");
        }
        assert_eq!((equal_power(-1.0), equal_power(2.0)), (0.0, 1.0));
    }

    /// Two comp segments meeting at tick 96 (0.5 s, sample 24 000) with a
    /// 10 ms crossfade: their gains (on a constant signal) keep unit power.
    #[test]
    fn crossfaded_clips_keep_constant_power_across_the_boundary() {
        let dc = Arc::new(Sample { name: "dc".into(), sample_rate: SR, left: vec![1.0; 96_000], right: vec![1.0; 96_000], markers: Vec::new() });
        let a = AudioClip { sample: dc, track: 2, start: 0, len: 96, gain: 1.0, xfade_out: 0.01, ..Default::default() };
        let b = AudioClip { start: 96, offset: 0.5, xfade_in: 0.01, xfade_out: 0.0, ..a.clone() };
        let map = TempoMap::default();
        let (la, _) = render_audio_clips(&[a], &map, SR, 0.0, 30_000);
        let (lb, _) = render_audio_clips(&[b], &map, SR, 0.0, 30_000);
        // Before the window only the first, after it only the second.
        assert_eq!((la[23_700], lb[23_700]), (1.0, 0.0));
        assert_eq!((la[24_300], lb[24_300]), (0.0, 1.0));
        for k in 23_700..24_300 {
            let power = la[k] * la[k] + lb[k] * lb[k];
            assert!((power - 1.0).abs() < 1e-5, "{k}: {power}");
        }
        // Centered on the boundary.
        assert!((la[24_000] - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-2);
        assert!((lb[24_000] - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-2);
    }

    /// The offline render (flattening) gives what the engine plays, whatever
    /// the block size; a crossfade before the file start reads silence.
    #[test]
    fn offline_clip_render_matches_playback() {
        let n = 48_000;
        let left: Vec<f32> = (0..n).map(|i| (i as f32 * 0.003).sin() * 0.8).collect();
        let sample = Sample { name: "sine".into(), sample_rate: 44_100.0, right: left.clone(), left, markers: Vec::new() };
        let a = AudioClip { len: 96, start: 48, xfade_out: 0.02, ..audio_clip(sample.clone(), 48, 0.0, 0.9) };
        let b = AudioClip { start: 144, len: 120, offset: 0.3, xfade_in: 0.02, xfade_out: 0.02, ..a.clone() };
        let c = AudioClip { start: 264, len: 100, offset: 0.0, xfade_in: 0.02, xfade_out: 0.0, ..a.clone() };
        let clips = vec![a, b, c];
        let (offline, _) = render_audio_clips(&clips, &TempoMap::default(), SR, 0.0, 120_000);
        for block in [64, 256] {
            let played = render_audio(clips.clone(), 120_000, block);
            for (k, (x, y)) in played.iter().zip(&offline).enumerate() {
                assert!((x - y).abs() < 1e-5, "block {block}, {k}: {x} vs {y}");
            }
        }
        assert!(offline.iter().any(|&s| s.abs() > 0.5));
    }

    #[test]
    fn audio_clip_loops_and_reverses() {
        let left: Vec<f32> = (0..1_000).map(|i| (i + 1) as f32 / 1_000.0).collect();
        let sample = Sample { name: "rampe".into(), sample_rate: SR, right: left.clone(), left, markers: Vec::new() };
        let mut clip = audio_clip(sample, 0, 0.0, 1.0);
        clip.looped = true;
        let out = render_audio(vec![clip.clone()], 5_000, 128);
        assert!((out[2_500] - 0.501).abs() < 1e-5, "{}", out[2_500]);
        clip.looped = false;
        clip.reverse = true;
        let out = render_audio(vec![clip], 5_000, 128);
        assert!((out[500] - 0.5).abs() < 1e-5, "{}", out[500]);
        assert!(out[1_000..].iter().all(|&s| s == 0.0));
    }

    #[test]
    fn fade_curves_bend_the_fades() {
        let sample = Sample { name: "dc".into(), sample_rate: SR, left: vec![1.0; 48_000], right: vec![1.0; 48_000], markers: Vec::new() };
        let mut clip = audio_clip(sample, 0, 0.0, 1.0);
        clip.fade_in = 0.1;
        let linear = render_audio(vec![clip.clone()], 10_000, 256)[2_400];
        clip.fade_in_curve = 1.0;
        let slow = render_audio(vec![clip.clone()], 10_000, 256)[2_400];
        clip.fade_in_curve = -1.0;
        let fast = render_audio(vec![clip], 10_000, 256)[2_400];
        assert!((linear - 0.5).abs() < 0.01 && slow < 0.1 && fast > 0.8, "{linear} {slow} {fast}");
    }

    #[test]
    fn warp_map_markers_bend_the_beats() {
        use crate::sequence::{WarpMap, WarpMarker};
        let plain = WarpMap::new(120.0, Vec::new());
        assert_eq!(plain.secs_at(4.0), 2.0);
        // Beat 4 pinned to 3 s instead of 2 s.
        let m = WarpMap::new(120.0, vec![WarpMarker { secs: 0.0, beat: 0.0 }, WarpMarker { secs: 3.0, beat: 4.0 }]);
        assert!((m.secs_at(2.0) - 1.5).abs() < 1e-9);
        assert!((m.secs_at(6.0) - 4.0).abs() < 1e-9);
        for b in [-1.0, 0.5, 3.0, 7.0] {
            assert!((m.beat_at(m.secs_at(b)) - b).abs() < 1e-9);
        }
        // A marker running backwards is dropped.
        let bad = WarpMap::new(120.0, vec![WarpMarker { secs: 1.0, beat: 2.0 }, WarpMarker { secs: 2.0, beat: 1.0 }]);
        assert_eq!(bad.markers().len(), 1);
    }

    #[test]
    fn process_to_loop_end_stops_at_song_end() {
        let (mut engine, mut handle, _) = setup(&[]);
        let seq = Sequence { mode: PlayMode::Song, song_len: 384, ..Default::default() };
        handle.send(Command::SetSequence(Box::new(seq))).unwrap();
        handle.send(Command::Play).unwrap();
        let mut buf = vec![0.0; 2 * 1_000];
        let mut total = 0;
        loop {
            let n = engine.process_to_loop_end(&mut buf, 2);
            total += n;
            if n < 1_000 {
                break;
            }
        }
        assert_eq!(total, 96_000);
    }

    /// Moteur au canal sonde coupé, avec une entrée branchée ; renvoie le producteur.
    fn setup_input() -> (Engine, EngineHandle, InputProducer) {
        let (mut engine, mut handle, _) = setup(&[]);
        let (tx, rx) = input_channel(4_096);
        handle.send(Command::SetChannelMute { channel: 0, muted: true }).unwrap();
        handle.send(Command::SetInput(Some(Box::new(rx)))).unwrap();
        engine.process(&mut [], 2);
        (engine, handle, tx)
    }

    /// Rampe stéréo entrelacée : gauche = k / 10 000, droite = −gauche.
    fn ramp(frames: usize) -> Vec<f32> {
        (0..frames).flat_map(|k| [k as f32 / 10_000.0, -(k as f32) / 10_000.0]).collect()
    }

    #[test]
    fn monitor_sums_input_into_track() {
        let (mut engine, mut handle, mut tx) = setup_input();
        handle.send(Command::SetMonitor { track: 2, source: Some(InputSource::Stereo) }).unwrap();
        assert_eq!(tx.push(&[0.5; 256], 1), 256);
        let mut buf = vec![0.0; 2 * 256];
        engine.process(&mut buf, 2);
        assert_eq!(handle.take_input_peak(), [0.5, 0.5]);
        assert_eq!(handle.take_track_peak(2), [0.5, 0.5]);
        assert_eq!(handle.take_master_peak(), [0.5, 0.5]);
        assert!(buf.iter().all(|&s| s == 0.5));

        // Sans écoute : le niveau d'entrée reste mesuré, la piste se tait.
        handle.send(Command::SetMonitor { track: 2, source: None }).unwrap();
        tx.push(&[0.25; 256], 1);
        engine.process(&mut buf, 2);
        assert_eq!(handle.take_input_peak(), [0.25, 0.25]);
        assert_eq!(handle.take_track_peak(2), [0.0, 0.0]);

        handle.send(Command::SetInput(None)).unwrap();
        engine.process(&mut [], 2);
        assert!(matches!(handle.garbage.pop(), Some(Garbage::Input(_))), "l'ancienne entrée revient");
    }

    #[test]
    fn recording_returns_samples_and_start_tick() {
        let (mut engine, mut handle, mut tx) = setup_input();
        handle.send(Command::Locate(96.0)).unwrap();
        handle.send(Command::Play).unwrap();
        run(&mut engine, 500, 250);
        let rec = Box::new(RecordBuffer::with_duration(1.0, SR));
        handle.send(Command::Record { track: 3, armed: true, buffer: Some(rec) }).unwrap();
        let data = ramp(1_000);
        assert_eq!(tx.push(&data, 2), 1_000);
        run(&mut engine, 1_000, 250);
        handle.send(Command::Record { track: 3, armed: false, buffer: None }).unwrap();
        // Les autres déchets ne font pas disparaître l'enregistrement.
        handle.send(Command::SetSequence(Box::default())).unwrap();
        engine.process(&mut [], 2);
        handle.collect_garbage();

        let recs = handle.take_recordings();
        assert_eq!(recs.len(), 1);
        let rec = &recs[0];
        assert_eq!((rec.len, rec.track, rec.sample_rate), (1_000, 3, SR));
        // 250 échantillons par tick : 500 échantillons après le tick 96.
        assert!((rec.start_tick - 98.0).abs() < 1e-9, "{}", rec.start_tick);
        let left: Vec<f32> = data.iter().step_by(2).copied().collect();
        let right: Vec<f32> = data.iter().skip(1).step_by(2).copied().collect();
        assert_eq!(rec.left[..1_000], left[..]);
        assert_eq!(rec.right[..1_000], right[..]);
        assert!(handle.take_recordings().is_empty());
    }

    #[test]
    fn full_record_buffer_stops_writing_and_returns_on_stop() {
        let (mut engine, mut handle, mut tx) = setup_input();
        let rec = Box::new(RecordBuffer::new(300, SR));
        handle.send(Command::Record { track: 1, armed: true, buffer: Some(rec) }).unwrap();
        handle.send(Command::Play).unwrap();
        let data = ramp(1_000);
        tx.push(&data, 2);
        run(&mut engine, 1_000, 256);
        handle.send(Command::Stop).unwrap();
        engine.process(&mut [], 2);
        let recs = handle.take_recordings();
        assert_eq!(recs.len(), 1);
        assert_eq!((recs[0].len, recs[0].start_tick), (300, 0.0));
        assert_eq!(recs[0].left[299], data[2 * 299]);
        // L'enregistrement est terminé : relancer la lecture n'écrit plus rien.
        handle.send(Command::Play).unwrap();
        tx.push(&data, 2);
        run(&mut engine, 1_000, 256);
        assert!(handle.take_recordings().is_empty());
    }

    #[test]
    fn a_take_grows_into_chunks_sent_while_it_records() {
        let (mut engine, mut handle, mut tx) = setup_input();
        assert_eq!(handle.record_room(1), None);
        let rec = Box::new(RecordBuffer::new(300, SR));
        handle.send(Command::Record { track: 1, armed: true, buffer: Some(rec) }).unwrap();
        handle.send(Command::Play).unwrap();
        let data = ramp(2_000);
        tx.push(&data[..2 * 256], 2);
        run(&mut engine, 256, 256);
        assert_eq!(handle.record_room(1), Some(300 - 256));
        assert_eq!(handle.record_room(2), None);
        // Two chunks while it records; one of the wrong format comes back.
        handle.send(Command::RecordChunk { track: 1, chunk: Box::new(RecordChunk::new(500, false)) }).unwrap();
        handle.send(Command::RecordChunk { track: 1, chunk: Box::new(RecordChunk::new(500, true)) }).unwrap();
        handle.send(Command::RecordChunk { track: 1, chunk: Box::new(RecordChunk::new(700, false)) }).unwrap();
        handle.send(Command::RecordChunk { track: 4, chunk: Box::new(RecordChunk::new(10, false)) }).unwrap();
        tx.push(&data[2 * 256..], 2);
        run(&mut engine, 2_000 - 256, 256);
        // 300 + 500 + 700 frames: the take is full.
        assert_eq!(handle.record_room(1), Some(0));
        handle.send(Command::Stop).unwrap();
        engine.process(&mut [], 2);
        assert_eq!(handle.record_room(1), None);
        let mut recs = handle.take_recordings();
        assert_eq!(recs.len(), 1);
        let rec = &mut recs[0];
        assert_eq!((rec.len, rec.capacity(), rec.chunks.len()), (1_500, 1_500, 2));
        rec.flatten();
        assert!(rec.chunks.is_empty() && rec.left.len() >= rec.len && rec.right.len() >= rec.len);
        // The whole take reads in order across the chunk boundaries.
        for i in 0..rec.len {
            assert_eq!((rec.left[i], rec.right[i]), (data[2 * i], data[2 * i + 1]), "frame {i}");
        }
        rec.shrink_for_reuse(300);
        assert_eq!((rec.capacity(), rec.len), (300, 0));
    }

    #[test]
    fn nothing_is_recorded_while_stopped() {
        let (mut engine, mut handle, mut tx) = setup_input();
        let rec = Box::new(RecordBuffer::new(4_800, SR));
        handle.send(Command::Record { track: 1, armed: true, buffer: Some(rec) }).unwrap();
        tx.push(&ramp(1_000), 2);
        run(&mut engine, 1_000, 256);
        assert!(handle.take_input_peak()[0] > 0.0, "l'entrée est lue");
        handle.send(Command::Record { track: 1, armed: false, buffer: None }).unwrap();
        engine.process(&mut [], 2);
        let recs = handle.take_recordings();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].len, 0);
    }

    #[test]
    fn audition_plays_outside_the_mixer() {
        let (mut engine, mut handle) = Engine::new(SR, Vec::new(), Sequence::default());
        let dc = Arc::new(Sample { name: "dc".into(), sample_rate: SR, left: vec![0.5; 4_800], right: vec![0.5; 4_800], markers: Vec::new() });
        handle.send(Command::Audition(Some(Box::new(Audition::new(dc, 0, 4_800, None, 1.0))))).unwrap();
        let mut buf = vec![0.0; 2 * 1_024];
        engine.process(&mut buf, 2);
        assert!((buf[2 * 1_000] - 0.5).abs() < 1e-4, "{}", buf[2_000]);
        assert_eq!(handle.take_master_peak(), [0.0, 0.0], "pas dans le mixer");
        assert!(handle.audition_position().is_some_and(|p| (p - 1_024.0).abs() < 1.0));
        // Arrêt en fondu, puis la voix revient à l'interface.
        handle.send(Command::Audition(None)).unwrap();
        engine.process(&mut buf, 2);
        assert_eq!(buf[2 * 1_000], 0.0);
        assert_eq!(handle.audition_position(), None);
        assert!(matches!(handle.garbage.pop(), Some(Garbage::Audition(_))));
    }

    #[test]
    fn audition_on_beat_waits_for_the_next_beat() {
        let (mut engine, mut handle) = Engine::new(SR, Vec::new(), Sequence::default());
        let dc = Arc::new(Sample { name: "dc".into(), sample_rate: SR, left: vec![0.5; 48_000], right: vec![0.5; 48_000], markers: Vec::new() });
        // Stopped: it plays at once.
        handle.send(Command::Audition(Some(Box::new(Audition::new(dc.clone(), 0, 48_000, None, 1.0).on_next_beat())))).unwrap();
        let mut buf = vec![0.0; 2 * 512];
        engine.process(&mut buf, 2);
        assert!(buf[2 * 500] > 0.4);
        // Playing: silent until the next beat.
        handle.send(Command::Play).unwrap();
        engine.process(&mut buf, 2);
        let beat = engine.frames_to(PPQ as f64).max(0.0) as usize;
        handle.send(Command::Audition(Some(Box::new(Audition::new(dc, 0, 48_000, None, 1.0).on_next_beat())))).unwrap();
        let mut long = vec![0.0; 2 * 48_000];
        engine.process(&mut long, 2);
        assert!(beat > 1_000, "{beat}");
        assert_eq!(long[2 * (beat - 600)], 0.0, "waits for the beat");
        assert!(long[2 * (beat + 600)] > 0.4, "then plays");
        handle.send(Command::AuditionGain(0.5)).unwrap();
        engine.process(&mut buf, 2);
        assert!((buf[2 * 500] - 0.25).abs() < 1e-3, "{}", buf[2 * 500]);
    }

    fn limiter() -> Box<dyn Effect> {
        let mut fx = crate::registry::create_effect("limiter").unwrap();
        fx.set_sample_rate(SR);
        fx
    }

    /// Indice de la crête.
    fn argmax(x: &[f32]) -> usize {
        (0..x.len()).max_by(|&a, &b| x[a].abs().total_cmp(&x[b].abs())).unwrap()
    }

    /// Sortie gauche en duplex, l'entrée étant une impulsion de 0,25 à la première trame.
    fn duplex_impulse(engine: &mut Engine, frames: usize) -> Vec<f32> {
        let mut left = Vec::new();
        let mut out = vec![0.0; 2 * 64];
        while left.len() < frames {
            let mut input = vec![0.0f32; 2 * 64];
            if left.is_empty() {
                input[..2].fill(0.25);
            }
            engine.process_duplex(&input, 2, &mut out, 2);
            left.extend(out.iter().step_by(2));
        }
        left
    }

    #[test]
    fn low_latency_monitoring_skips_lookahead_and_compensation() {
        let (mut engine, mut handle, _) = setup(&[]);
        let lat = limiter().latency();
        assert!(lat > 64);
        handle.send(Command::SetChannelMute { channel: 0, muted: true }).unwrap();
        handle.send(Command::SetInsert { track: 2, slot: 0, effect: Some(limiter()) }).unwrap();
        handle.send(Command::SetInsert { track: 3, slot: 0, effect: Some(limiter()) }).unwrap();
        handle.send(Command::SetMonitor { track: 2, source: Some(InputSource::Stereo) }).unwrap();
        // Compensation complète : la voix revient avec l'anticipation du limiteur.
        let out = duplex_impulse(&mut engine, 4_000);
        assert_eq!(argmax(&out), lat);
        assert_eq!(handle.latency(), lat);
        // Faible latence : la voix ressort dans le même cycle, sans compensation.
        handle.send(Command::SetLowLatencyMonitoring(true)).unwrap();
        let out = duplex_impulse(&mut engine, 4_000);
        assert_eq!(argmax(&out), 0);
        assert!((out[0] - 0.25).abs() < 1e-6, "{}", out[0]);
        assert_eq!(handle.latency(), lat, "la piste 3 et son limiteur restent compensés");
        // Retour à la normale.
        handle.send(Command::SetLowLatencyMonitoring(false)).unwrap();
        assert_eq!(argmax(&duplex_impulse(&mut engine, 4_000)), lat);
    }

    /// Pitch correction is made to be heard while singing: low-latency
    /// monitoring keeps it, with its own latency, instead of skipping it.
    #[test]
    fn low_latency_monitoring_keeps_pitch_correction() {
        let (mut engine, mut handle, _) = setup(&[]);
        let tune = || {
            let mut fx = crate::registry::create_effect("autotune").unwrap();
            fx.set_sample_rate(SR);
            fx
        };
        let lat = tune().latency();
        assert!(lat > 0 && tune().keep_when_monitoring());
        handle.send(Command::SetChannelMute { channel: 0, muted: true }).unwrap();
        handle.send(Command::SetInsert { track: 2, slot: 0, effect: Some(tune()) }).unwrap();
        handle.send(Command::SetMonitor { track: 2, source: Some(InputSource::Stereo) }).unwrap();
        handle.send(Command::SetLowLatencyMonitoring(true)).unwrap();
        let out = duplex_impulse(&mut engine, 8_000);
        assert_eq!(argmax(&out), lat, "the voice goes through the corrector");
        assert_eq!(handle.latency(), lat);
    }

    #[test]
    fn duplex_input_is_recorded_dry_from_each_source() {
        let (mut engine, mut handle, _) = setup(&[]);
        handle.send(Command::SetChannelMute { channel: 0, muted: true }).unwrap();
        handle.send(Command::SetInsert { track: 2, slot: 0, effect: Some(limiter()) }).unwrap();
        handle.send(Command::SetMonitor { track: 2, source: Some(InputSource::Left) }).unwrap();
        let left_buf = RecordBuffer::for_source(InputSource::Left, 1.0, SR);
        let right_buf = RecordBuffer::for_source(InputSource::Right, 1.0, SR);
        handle.send(Command::Record { track: 2, armed: true, buffer: Some(Box::new(left_buf)) }).unwrap();
        handle.send(Command::Record { track: 4, armed: true, buffer: Some(Box::new(right_buf)) }).unwrap();
        handle.send(Command::Play).unwrap();
        let data = ramp(1_000);
        let mut out = vec![0.0; 2 * 1_000];
        engine.process_duplex(&data, 2, &mut out, 2);
        handle.send(Command::Stop).unwrap();
        engine.process(&mut [], 2);
        let mut recs = handle.take_recordings();
        recs.sort_by_key(|r| r.track);
        assert_eq!(recs.len(), 2);
        let left: Vec<f32> = data.iter().step_by(2).copied().collect();
        let right: Vec<f32> = data.iter().skip(1).step_by(2).copied().collect();
        assert_eq!((recs[0].track, recs[0].is_mono(), recs[0].len), (2, true, 1_000));
        assert_eq!(recs[0].left[..1_000], left[..], "prise sèche : ni limiteur ni retard");
        assert_eq!((recs[1].track, recs[1].len), (4, 1_000));
        assert_eq!(recs[1].left[..1_000], right[..]);
    }

    #[test]
    fn loop_recording_marks_each_pass_and_aligns_takes() {
        let (mut engine, mut handle, _) = setup(&[]);
        handle.send(Command::SetChannelMute { channel: 0, muted: true }).unwrap();
        let seq = Sequence { mode: PlayMode::Song, loop_range: Some((96, 192)), ..Default::default() };
        handle.send(Command::SetSequence(Box::new(seq))).unwrap();
        // Un limiteur ailleurs : la compensation retarde ce qu'on entend.
        handle.send(Command::SetInsert { track: 3, slot: 0, effect: Some(limiter()) }).unwrap();
        handle.send(Command::Locate(48.0)).unwrap();
        let rec = RecordBuffer::with_duration(2.0, SR);
        handle.send(Command::Record { track: 1, armed: true, buffer: Some(Box::new(rec)) }).unwrap();
        handle.send(Command::Play).unwrap();
        // 250 trames par tick : pré-roll de 144 ticks jusqu'à la fin de boucle,
        // puis des tours de 96 ticks (24 000 trames).
        let input = vec![0.1f32; 2 * 128];
        let mut out = vec![0.0; 2 * 128];
        for _ in 0..508 {
            engine.process_duplex(&input, 2, &mut out, 2);
        }
        handle.send(Command::Stop).unwrap();
        engine.process(&mut [], 2);
        let recs = handle.take_recordings();
        let rec = &recs[0];
        assert_eq!((rec.len, rec.start_tick, rec.samples_per_tick), (508 * 128, 48.0, 250.0));
        assert_eq!(rec.mix_latency, limiter().latency());
        assert_eq!(rec.jumps, [(36_000, 96.0), (60_000, 96.0)]);
        // 480 trames d'aller-retour du pilote en plus de la compensation.
        let shift = rec.mix_latency + 480;
        let takes = take_slices(rec.len, rec.start_tick, &rec.jumps, shift, rec.samples_per_tick, None, 1);
        assert_eq!(takes.len(), 3);
        assert_eq!(takes[0].tick, 48.0 - shift as f64 / 250.0);
        assert_eq!((takes[1].from, takes[1].to - takes[1].from, takes[1].tick), (36_000 + shift, 24_000, 96.0));
        assert_eq!((takes[2].from, takes[2].tick), (60_000 + shift, 96.0));
    }

    #[test]
    fn calibration_measures_a_simulated_loopback() {
        let (mut engine, mut handle, _) = setup(&[]);
        handle.send(Command::SetChannelMute { channel: 0, muted: true }).unwrap();
        let cal = RecordBuffer::with_duration(CALIBRATION_SECONDS, SR);
        handle.send(Command::Calibrate(Some(Box::new(cal)))).unwrap();
        // Câble de bouclage : ce qui sort revient à l'entrée 1 234 trames plus tard.
        let delay = 1_234;
        let mut line: std::collections::VecDeque<f32> = std::iter::repeat_n(0.0, 2 * delay).collect();
        let mut out = vec![0.0; 2 * 128];
        let mut cal = None;
        for _ in 0..2_000 {
            let input: Vec<f32> = line.drain(..2 * 128).collect();
            engine.process_duplex(&input, 2, &mut out, 2);
            line.extend(&out);
            cal = handle.take_calibration();
            if cal.is_some() {
                break;
            }
        }
        let cal = cal.expect("calibrage rendu");
        assert!(cal.is_full());
        assert_eq!(measure_round_trip(&cal.left[..cal.len], SR), Some(delay));
        // Plus de clics une fois le calibrage terminé.
        engine.process_duplex(&[0.0; 2 * 128], 2, &mut out, 2);
        assert!(out.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn capture_records_the_input_without_transport_and_comes_back() {
        let (mut engine, mut handle, _) = setup(&[]);
        handle.send(Command::Capture(Some(Box::new(RecordBuffer::new(1_000, SR))))).unwrap();
        let mut out = vec![0.0; 2 * 128];
        let input: Vec<f32> = (0..2 * 128).map(|i| if i % 2 == 0 { 0.25 } else { -0.5 }).collect();
        // Stopped transport: the input still goes in, until the buffer is full.
        let mut cap = None;
        for _ in 0..20 {
            engine.process_duplex(&input, 2, &mut out, 2);
            cap = handle.take_capture();
            if cap.is_some() {
                break;
            }
        }
        let cap = cap.expect("full capture returned");
        assert!(cap.is_full() && cap.len == 1_000);
        assert!(cap.left[..cap.len].iter().all(|&v| (v - 0.25).abs() < 1e-6), "{:?}", &cap.left[..4]);
        assert!(cap.right[..cap.len].iter().all(|&v| (v + 0.5).abs() < 1e-6));
        // Stopped early: what was recorded comes back.
        handle.send(Command::Capture(Some(Box::new(RecordBuffer::new(48_000, SR))))).unwrap();
        engine.process_duplex(&input, 2, &mut out, 2);
        engine.process_duplex(&input, 2, &mut out, 2);
        handle.send(Command::Capture(None)).unwrap();
        engine.process_duplex(&input, 2, &mut out, 2);
        let cap = handle.take_capture().expect("stopped capture returned");
        assert_eq!(cap.len, 256);
    }

    #[test]
    fn rearming_a_track_returns_its_take_and_others_keep_recording() {
        let (mut engine, mut handle, mut tx) = setup_input();
        for track in [1, 2] {
            let buffer = Some(Box::new(RecordBuffer::new(4_800, SR)));
            handle.send(Command::Record { track, armed: true, buffer }).unwrap();
        }
        handle.send(Command::Play).unwrap();
        tx.push(&ramp(256), 2);
        run(&mut engine, 256, 256);
        handle.send(Command::Record { track: 1, armed: false, buffer: None }).unwrap();
        tx.push(&ramp(256), 2);
        run(&mut engine, 256, 256);
        let recs = handle.take_recordings();
        assert_eq!(recs.iter().map(|r| (r.track, r.len)).collect::<Vec<_>>(), [(1, 256)]);
        handle.send(Command::Stop).unwrap();
        engine.process(&mut [], 2);
        let recs = handle.take_recordings();
        assert_eq!(recs.iter().map(|r| (r.track, r.len)).collect::<Vec<_>>(), [(2, 512)]);
    }

    #[test]
    fn cpu_meters_follow_tracks_inserts_and_channels() {
        for workers in [0, 2] {
            let (mut engine, mut handle, _) = setup(&[0, 1, 2, 3]);
            handle.send(Command::AddChannel(Box::new(Channel::new("p2", Box::new(Probe::default())).with_track(2)))).unwrap();
            engine.set_worker_threads(workers);
            assert_eq!(handle.worker_threads(), workers);
            let mut reverb = crate::registry::create_effect("reverb").unwrap();
            reverb.set_sample_rate(SR);
            handle.send(Command::SetInsert { track: 1, slot: 0, effect: Some(reverb) }).unwrap();
            handle.send(Command::Play).unwrap();
            run(&mut engine, 48_000, 256);
            assert!(handle.track_cpu(1) > 0.0 && handle.insert_cpu(1, 0) > 0.0, "{workers} threads");
            assert!(handle.insert_cpu(1, 0) <= handle.track_cpu(1));
            assert_eq!(handle.insert_cpu(1, 1), 0.0, "emplacement vide");
            assert_eq!(handle.insert_cpu(2, 0), 0.0);
            assert!(handle.channel_cpu(0) > 0.0 && handle.channel_cpu(1) > 0.0);
            assert_eq!(handle.channel_cpu(2), 0.0, "pas de canal");
            assert_eq!(handle.insert_cpu(1, INSERT_SLOTS), 0.0);
        }
    }

    #[test]
    fn rapid_routing_changes_while_running_never_block() {
        // Ordres de routage et sorties qui changent à chaque bloc, sur plusieurs threads.
        let (mut engine, mut handle, _) = setup(&[0, 2, 4, 6, 8, 10, 12, 14]);
        handle.send(Command::AddChannel(Box::new(Channel::new("p2", Box::new(Probe::default())).with_track(2)))).unwrap();
        engine.set_worker_threads(3);
        handle.send(Command::Play).unwrap();
        for block in 0..2_000usize {
            let order: Box<[usize]> = match block % 4 {
                0 => crate::mixer::default_order(DEFAULT_TRACKS),
                1 => (1..DEFAULT_TRACKS).collect(),
                2 => [3, 1, 2, 1, 40].into(),
                _ => Box::new([]),
            };
            handle.send(Command::SetRoutingOrder(order)).unwrap();
            let track = 1 + block % 5;
            handle.send(Command::SetTrackOutput { track, output: (block * 7) % DEFAULT_TRACKS }).unwrap();
            handle.send(Command::SetTrackSend { track, slot: 0, target: (block * 3) % DEFAULT_TRACKS, amount: 0.5 }).unwrap();
            handle.send(Command::SetInsertSidechain { track: 2, slot: 0, key: Some(1 + block % 4) }).unwrap();
            run(&mut engine, 1 + block % 300, 256);
            handle.collect_garbage();
        }
    }
}
