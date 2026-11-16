use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

use ponzik_dsp::util::balance_gains;

use crate::audition::Audition;
use crate::effect::Effect;
use crate::input::{InputConsumer, RecordBuffer};
use crate::instrument::Instrument;
use crate::meter::{PeakMeter, peak};
use crate::mixer::{INSERT_SLOTS, MASTER, Mixer, SEND_SLOTS, TRACKS};
use crate::note_fx::NoteFx;
use crate::rack::{Channel, NOTE_FX_SLOTS};
use crate::ring::{self, Consumer, Producer};
use crate::sequence::{AutoTarget, PPQ, PlayMode, SeqEvent, Sequence};

/// Taille max d'un bloc de traitement (l'automation est évaluée à chaque bloc).
const MAX_BLOCK: usize = 256;
/// Clips audio suivis d'un bloc à l'autre ; au-delà, la position est recalculée
/// depuis le tick à chaque bloc.
const MAX_AUDIO_VOICES: usize = 4_096;
/// Fondu minimal des clips audio, en secondes.
const MIN_FADE: f32 = 0.002;
const COMMAND_QUEUE: usize = 4_096;
const GARBAGE_QUEUE: usize = 1_024;
const MAX_EVENTS: usize = 8_192;
pub const MAX_CHANNELS: usize = 128;
/// Échantillons du master gardés pour l'oscilloscope (≈ 0,7 s à 24 kHz).
const SCOPE_QUEUE: usize = 16_384;
const SCOPE_DECIMATION: u32 = 2;
/// Plus grande latence compensée par le mixer, en secondes.
const MAX_LATENCY_SECONDS: f32 = 1.0;

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
    SetBpm(f32),
    SetSequence(Box<Sequence>),
    AddChannel(Box<Channel>),
    RemoveChannel(usize),
    ReplaceInstrument { channel: usize, instrument: Box<dyn Instrument> },
    SetChannelGain { channel: usize, gain: f32 },
    SetChannelPan { channel: usize, pan: f32 },
    SetChannelMute { channel: usize, muted: bool },
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
    SetInsert { track: usize, slot: usize, effect: Option<Box<dyn Effect>> },
    SetInsertBypass { track: usize, slot: usize, bypass: bool },
    /// Branche la piste clé d'un insert (`None` : débranche). L'interface doit
    /// aussi envoyer le nouvel ordre de routage ([`Command::SetRoutingOrder`]) ;
    /// une clé pas encore traitée dans le bloc est lue comme du silence.
    SetInsertSidechain { track: usize, slot: usize, key: Option<usize> },
    SetEffectParam { track: usize, slot: usize, id: u32, value: f32 },
    SetMetronome(bool),
    /// Branche (ou débranche) l'entrée audio ; l'ancienne repart en [`Garbage::Input`].
    SetInput(Option<Box<InputConsumer>>),
    /// Écoute de l'entrée en direct, ajoutée à l'entrée de la piste (avant ses inserts).
    SetMonitor { track: Option<usize> },
    /// Arme l'enregistrement de l'entrée dans `buffer`, préalloué par l'interface ;
    /// il n'est rempli que transport lancé. Désarmer, réarmer ou arrêter le
    /// transport rend le tampon en cours via [`Garbage::Recording`]. Un seul
    /// enregistrement à la fois ; `armed` sans tampon désarme.
    Record { track: usize, armed: bool, buffer: Option<Box<RecordBuffer>> },
    /// Pré-écoute de l'Atelier, ajoutée à la sortie hors du mixer : remplace la
    /// voix en cours (rendue en [`Garbage::Audition`]) ; `None` l'arrête en fondu.
    Audition(Option<Box<Audition>>),
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
            Command::SetChannelGain { .. } => "SetChannelGain",
            Command::SetChannelPan { .. } => "SetChannelPan",
            Command::SetChannelMute { .. } => "SetChannelMute",
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
            Command::SetInsert { .. } => "SetInsert",
            Command::SetInsertBypass { .. } => "SetInsertBypass",
            Command::SetInsertSidechain { .. } => "SetInsertSidechain",
            Command::SetEffectParam { .. } => "SetEffectParam",
            Command::SetMetronome(_) => "SetMetronome",
            Command::SetInput(_) => "SetInput",
            Command::SetMonitor { .. } => "SetMonitor",
            Command::Record { .. } => "Record",
            Command::Audition(_) => "Audition",
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
    Routing(Box<[usize]>),
    Input(Box<InputConsumer>),
    /// Enregistrement terminé : gardé à part par [`EngineHandle::take_recordings`].
    Recording(Box<RecordBuffer>),
    Audition(Box<Audition>),
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
    /// Pire charge DSP depuis la dernière lecture : temps de calcul / durée du bloc.
    load: AtomicU32,
    /// Latence totale au master, en échantillons.
    latency: AtomicU32,
    /// Position de la pré-écoute en images de l'échantillon (bits d'un `f64`, −1 : rien).
    audition: AtomicU64,
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
    sample_rate: f32,
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
            if let Garbage::Recording(rec) = g {
                self.recordings.push(rec);
            }
        }
    }

    /// Enregistrements terminés depuis le dernier appel (tampons vides compris).
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
    /// Ancre de la position : au `clock` `anchor_clock`, on était au tick `anchor_tick`.
    /// Recalculer depuis l'ancre évite toute dérive d'arrondi.
    anchor_tick: f64,
    anchor_clock: u64,
    metronome: bool,
    /// Pré-compte : ticks restants et durée totale ; la lecture démarre à 0.
    count_in: f64,
    count_len: f64,
    click_phase: f32,
    click_env: f32,
    click_hz: f32,

    events: Vec<SeqEvent>,
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
    /// Piste qui reçoit l'entrée en direct.
    monitor: Option<usize>,
    /// Enregistrement armé.
    recording: Option<Box<RecordBuffer>>,
    /// Enregistrement que la file de retour pleine n'a pas pu prendre ; réessayé au bloc suivant.
    returning: Option<Box<RecordBuffer>>,
    /// Pré-écoute de l'Atelier.
    audition: Option<Box<Audition>>,
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

impl Engine {
    pub fn new(sample_rate: f32, mut channels: Vec<Channel>, sequence: Sequence) -> (Engine, EngineHandle) {
        for ch in &mut channels {
            ch.instrument.set_sample_rate(sample_rate);
            for fx in ch.note_fx.iter_mut().flatten() {
                fx.set_sample_rate(sample_rate);
                fx.set_tempo(120.0);
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
            track_peaks: (0..TRACKS).map(|_| PeakMeter::default()).collect(),
            input_peak: PeakMeter::default(),
            load: AtomicU32::new(0),
            latency: AtomicU32::new(0),
            audition: AtomicU64::new((-1f64).to_bits()),
        });
        let mut voices = Vec::with_capacity(MAX_AUDIO_VOICES);
        voices.resize(sequence.audio_clips.len().min(MAX_AUDIO_VOICES), AudioVoice::default());
        let engine = Engine {
            sample_rate,
            channels,
            mixer: Mixer::new(MAX_BLOCK, (sample_rate.max(1.0) * MAX_LATENCY_SECONDS) as usize),
            sequence: Box::new(sequence),
            commands: rx,
            state: state.clone(),
            scope: scope_tx,
            scope_phase: 0,
            garbage: trash_tx,
            clock: 0,
            playing: false,
            bpm: 120.0,
            pos: 0.0,
            anchor_tick: 0.0,
            anchor_clock: 0,
            metronome: false,
            count_in: 0.0,
            count_len: 0.0,
            click_phase: 0.0,
            click_env: 0.0,
            click_hz: 0.0,
            events: Vec::with_capacity(MAX_EVENTS),
            scratch_l: vec![0.0; MAX_BLOCK],
            scratch_r: vec![0.0; MAX_BLOCK],
            voices,
            audio_next: f64::NAN,
            input: None,
            input_l: vec![0.0; MAX_BLOCK],
            input_r: vec![0.0; MAX_BLOCK],
            monitor: None,
            recording: None,
            returning: None,
            audition: None,
        };
        let handle = EngineHandle {
            commands: tx,
            state,
            scope: scope_rx,
            garbage: trash_rx,
            recordings: Vec::new(),
            sample_rate,
        };
        (engine, handle)
    }

    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    /// Accès direct au mixer, pour préparer l'état avant le premier bloc.
    pub fn mixer_mut(&mut self) -> &mut Mixer {
        &mut self.mixer
    }

    /// Échantillons par tick au tempo courant.
    fn spt(&self) -> f64 {
        self.sample_rate as f64 * 60.0 / (self.bpm * PPQ as f64)
    }

    fn anchor(&mut self) {
        self.anchor_tick = self.pos;
        self.anchor_clock = self.clock;
    }

    fn trash(&mut self, g: Garbage) {
        // File pleine : l'objet est libéré ici, en dernier recours ; un
        // enregistrement est gardé pour le bloc suivant s'il y a de la place.
        if let Err(Garbage::Recording(rec)) = self.garbage.push(g)
            && self.returning.is_none()
        {
            self.returning = Some(rec);
        }
    }

    /// Rend l'enregistrement en cours à l'interface.
    fn finish_recording(&mut self) {
        if let Some(rec) = self.recording.take() {
            self.trash(Garbage::Recording(rec));
        }
    }

    /// Remplit `out` (entrelacé, `channels` canaux) — appelé par le thread audio.
    pub fn process(&mut self, out: &mut [f32], channels: usize) {
        self.run(out, channels, false);
    }

    /// Comme [`Engine::process`], mais s'arrête pile à la fin de la boucle du
    /// morceau au lieu de reboucler (export) ; renvoie le nombre d'images écrites,
    /// le reste de `out` n'est pas touché.
    pub fn process_to_loop_end(&mut self, out: &mut [f32], channels: usize) -> usize {
        self.run(out, channels, true)
    }

    fn run(&mut self, out: &mut [f32], channels: usize, stop_at_end: bool) -> usize {
        let started = Instant::now();
        self.drain_commands();
        let channels = channels.max(1);
        let frames = out.len() / channels;
        let mut done = 0;
        while done < frames {
            let mut n = (frames - done).min(MAX_BLOCK);
            if self.playing {
                let end = self.sequence.loop_end();
                if let Some(end) = end
                    && self.pos >= end - 1e-9
                {
                    if stop_at_end {
                        break;
                    }
                    self.pos = self.sequence.loop_start();
                    self.anchor();
                }
                // Avant de couper le bloc : l'automation peut changer le tempo.
                self.apply_automation();
                if let Some(end) = end {
                    // On coupe le bloc à la fin de la boucle du morceau.
                    // Tolérance : une position calculée un rien avant la fin ne doit pas
                    // ajouter un échantillon.
                    let left = ((end - self.pos) * self.spt() - 1e-6).ceil().max(1.0) as usize;
                    n = n.min(left);
                }
            } else if self.count_in > 0.0 {
                // Le bloc s'arrête pile à la fin du pré-compte.
                let left = (self.count_in * self.spt() - 1e-6).ceil().max(1.0) as usize;
                n = n.min(left);
            }
            self.process_chunk(n);
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
        if frames > 0 {
            let budget = frames as f32 / self.sample_rate;
            let load = started.elapsed().as_secs_f32() / budget;
            self.state.load.fetch_max(load.to_bits(), Ordering::Relaxed);
        }
        done
    }

    /// Applique les courbes actives à la position courante (début de bloc).
    fn apply_automation(&mut self) {
        let pos = self.pos;
        let mut tempo = None;
        for lane in &self.sequence.automation {
            let Some(v) = self.sequence.automation_tick(lane.scope, pos).and_then(|t| lane.value_at(t)) else {
                continue;
            };
            if !v.is_finite() {
                continue;
            }
            match lane.target {
                AutoTarget::InstrumentParam { channel, id } => {
                    if let Some(ch) = self.channels.get_mut(channel)
                        && ch.instrument.get_param(id) != v
                    {
                        ch.instrument.set_param(id, v);
                    }
                }
                AutoTarget::EffectParam { track, slot, id } => {
                    if let Some(Some(fx)) = self.mixer.tracks.get_mut(track).and_then(|t| t.inserts.get_mut(slot))
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
                AutoTarget::Tempo => tempo = Some(v),
            }
        }
        if let Some(bpm) = tempo {
            let bpm = (bpm as f64).clamp(20.0, 999.0);
            if bpm != self.bpm {
                // Nouvelle ancre à la position courante : pas de saut.
                self.anchor();
                self.bpm = bpm;
                self.set_tempo();
            }
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

    fn process_chunk(&mut self, n: usize) {
        let spt = self.spt();
        let pos_start = self.pos;
        if self.playing {
            let to = pos_start + n as f64 / spt;
            self.sequence.collect(pos_start, to, &mut self.events);
        } else {
            self.events.clear();
        }
        let offset_of = |tick: f64| ((((tick - pos_start) * spt) - 1e-6).ceil().max(0.0) as usize).min(n - 1);

        self.mixer.clear(n);
        let mut i = 0;
        let mut next_event = 0;
        while i < n {
            let now = self.clock + i as u64;
            for ch in &mut self.channels {
                ch.release_due(now);
            }
            while let Some(ev) = self.events.get(next_event) {
                if offset_of(ev.tick) > i {
                    break;
                }
                let ev = *ev;
                next_event += 1;
                if let Some(ch) = self.channels.get_mut(ev.channel)
                    && !ch.muted
                {
                    let off_at = now + (ev.len.max(1) as f64 * spt).round().max(1.0) as u64;
                    ch.trigger(now, ev.key, ev.vel, off_at);
                }
            }
            for ch in &mut self.channels {
                ch.flush(now);
            }
            // Jusqu'au prochain événement (début ou fin de note).
            let mut end = n;
            if let Some(ev) = self.events.get(next_event) {
                end = end.min(offset_of(ev.tick).clamp(i + 1, n));
            }
            let next_off = self.channels.iter().filter_map(Channel::next_off).min();
            let next_fx = self.channels.iter().filter_map(Channel::next_fx_event).min();
            if let Some(at) = next_off.into_iter().chain(next_fx).min() {
                end = end.min((at.saturating_sub(self.clock) as usize).clamp(i + 1, n));
            }
            self.render_channels(i, end - i);
            i = end;
        }

        self.render_audio_clips(pos_start, n, spt);
        self.capture_input(pos_start, n);
        self.metronome_click(pos_start, n, spt);
        self.mixer.process(n, &self.state.track_peaks);
        self.feed_scope(n);
        self.clock += n as u64;
        if self.playing {
            self.pos = self.anchor_tick + (self.clock - self.anchor_clock) as f64 / spt;
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

    /// Clips audio (mode morceau) : lecture à la fréquence de l'échantillon,
    /// interpolation linéaire, début à l'échantillon près, fondus aux bords.
    fn render_audio_clips(&mut self, pos_start: f64, n: usize, spt: f64) {
        let song = self.playing && self.sequence.mode == PlayMode::Song;
        // Bloc dans la continuité du précédent : les clips reprennent où ils étaient
        // (même si le tempo a changé entre-temps).
        let contiguous = (pos_start - self.audio_next).abs() < 1e-6;
        let sr = self.sample_rate as f64;
        let to = pos_start + n as f64 / spt;
        let first = |tick: f64| (((tick - pos_start) * spt - 1e-6).ceil().max(0.0) as usize).min(n);
        for (idx, clip) in self.sequence.audio_clips.iter().enumerate() {
            let prev = self.voices.get(idx).copied().unwrap_or_default();
            let mut voice = AudioVoice::default();
            let sample = &*clip.sample;
            let len = sample.len();
            let (start, end) = (clip.start as f64, clip.start as f64 + clip.len as f64);
            if song && len > 0 && end > pos_start && start < to {
                let (k0, k1) = (first(start), first(end));
                let src_rate = sample.sample_rate.max(1.0) as f64;
                let ratio = src_rate / sr;
                let resume = prev.active && contiguous && k0 == 0;
                let (mut p, mut run) = if resume {
                    (prev.src, prev.run)
                } else {
                    let tick = pos_start + k0 as f64 / spt;
                    let secs = clip.offset.max(0.0) + (tick - start).max(0.0) * spt / sr;
                    // Après un saut, petit fondu d'entrée ; sinon (nouvelle séquence,
                    // début du clip) seul le fondu du clip compte.
                    (secs * src_rate, if contiguous { f64::MAX / 2.0 } else { 0.0 })
                };
                let origin = clip.offset.max(0.0) * src_rate;
                let fade_in = clip.fade_in.max(MIN_FADE) as f64 * sr;
                let fade_out = clip.fade_out.max(MIN_FADE) as f64 * sr;
                let end_at = (end - pos_start) * spt;
                let right = if sample.right.len() == len { &sample.right } else { &sample.left };
                let (tl, tr) = self.mixer.input(clip.track, n);
                for k in k0..k1 {
                    if p >= len as f64 {
                        break;
                    }
                    let i = p as usize;
                    let j = (i + 1).min(len - 1);
                    let frac = (p - i as f64) as f32;
                    let l = sample.left[i] + (sample.left[j] - sample.left[i]) * frac;
                    let r = right[i] + (right[j] - right[i]) * frac;
                    // En échantillons de sortie ; +1 : le premier échantillon n'est pas nul.
                    let since_start = (p - origin) / ratio + 1.0;
                    let until_end = (end_at - k as f64).min((len as f64 - p) / ratio);
                    let fade = (since_start.min(run + 1.0) / fade_in).min(1.0) * (until_end / fade_out).min(1.0);
                    let g = clip.gain * fade.max(0.0) as f32;
                    tl[k] += l * g;
                    tr[k] += r * g;
                    p += ratio;
                    run += 1.0;
                }
                voice = AudioVoice { active: k1 == n && p < len as f64, src: p, run };
            }
            if let Some(v) = self.voices.get_mut(idx) {
                *v = voice;
            }
        }
    }

    /// Lit l'entrée : crête, écoute sur la piste choisie, enregistrement si armé et en lecture.
    fn capture_input(&mut self, pos_start: f64, n: usize) {
        let Some(input) = self.input.as_mut() else { return };
        let (l, r) = (&mut self.input_l[..n], &mut self.input_r[..n]);
        input.read(l, r);
        self.state.input_peak.record(peak(l), peak(r));
        if let Some(track) = self.monitor {
            let (tl, tr) = self.mixer.input(track, n);
            for k in 0..n {
                tl[k] += l[k];
                tr[k] += r[k];
            }
        }
        if self.playing
            && let Some(rec) = self.recording.as_mut()
        {
            if rec.len == 0 {
                rec.start_tick = pos_start;
            }
            // Tampon plein : on cesse d'écrire, il sera rendu au désarmement.
            rec.write(l, r);
        }
    }

    fn render_channels(&mut self, offset: usize, len: usize) {
        for (idx, ch) in self.channels.iter_mut().enumerate() {
            let (l, r) = (&mut self.scratch_l[..len], &mut self.scratch_r[..len]);
            l.fill(0.0);
            r.fill(0.0);
            ch.instrument.render(l, r);
            if ch.muted {
                continue;
            }
            let (gl, gr) = balance_gains(ch.pan);
            let (gl, gr) = (gl * ch.gain, gr * ch.gain);
            for s in l.iter_mut() {
                *s *= gl;
            }
            for s in r.iter_mut() {
                *s *= gr;
            }
            if let Some(m) = self.state.channel_peaks.get(idx) {
                m.record(peak(l), peak(r));
            }
            let (tl, tr) = self.mixer.input(ch.track, offset + len);
            for k in 0..len {
                tl[offset + k] += l[k];
                tr[offset + k] += r[k];
            }
        }
    }

    /// Clic de métronome sur chaque temps, ajouté directement au master. Pendant
    /// le pré-compte, les temps se comptent depuis son début.
    fn metronome_click(&mut self, pos_start: f64, n: usize, spt: f64) {
        let pos_start = if self.count_in > 0.0 {
            self.count_len - self.count_in
        } else if self.playing && self.metronome {
            pos_start
        } else {
            self.click_env = 0.0;
            return;
        };
        let sr = self.sample_rate;
        let decay = (-1.0 / (0.012 * sr)).exp();
        let beat_ticks = PPQ as f64;
        let (l, r) = self.mixer.input(MASTER, n);
        for k in 0..n {
            let tick = pos_start + k as f64 / spt;
            let beat = (tick / beat_ticks).floor();
            if tick - beat * beat_ticks < 1.0 / spt {
                let bar_start = (beat as i64).rem_euclid(4) == 0;
                self.click_hz = if bar_start { 1_760.0 } else { 1_320.0 };
                self.click_env = 0.35;
                self.click_phase = 0.0;
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
        if let Some(rec) = self.returning.take() {
            self.trash(Garbage::Recording(rec));
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
                self.mixer.reset_effects();
                self.finish_recording();
            }
            Command::Locate(tick) => {
                self.pos = tick.max(0.0);
                self.anchor();
                self.silence_all();
            }
            Command::SetBpm(bpm) => {
                self.anchor();
                self.bpm = (bpm as f64).clamp(20.0, 999.0);
                self.set_tempo();
            }
            Command::SetSequence(seq) => {
                // Dans la capacité réservée : pas d'allocation.
                self.voices.clear();
                let voices = seq.audio_clips.len().min(self.voices.capacity());
                self.voices.resize(voices, AudioVoice::default());
                let old = std::mem::replace(&mut self.sequence, seq);
                self.trash(Garbage::Sequence(old));
            }
            Command::AddChannel(mut ch) => {
                if self.channels.len() < MAX_CHANNELS {
                    // Le canal sort de sa boîte ; la boîte et son bouche-trou repartent.
                    let channel = std::mem::replace(&mut *ch, Channel::new(String::new(), Box::new(Silence)));
                    self.channels.push(channel);
                }
                self.trash(Garbage::Channel(ch));
            }
            Command::RemoveChannel(idx) => {
                if idx < self.channels.len() {
                    let ch = self.channels.remove(idx);
                    self.trash(Garbage::Channel(Box::new(ch)));
                }
            }
            Command::ReplaceInstrument { channel, instrument } => {
                if let Some(ch) = self.channels.get_mut(channel) {
                    ch.silence();
                    let old = std::mem::replace(&mut ch.instrument, instrument);
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
            Command::SetChannelTrack { channel, track } => {
                if let Some(ch) = self.channels.get_mut(channel) {
                    ch.track = track.min(TRACKS - 1);
                }
            }
            Command::SetInstrumentParam { channel, id, value } => {
                if let Some(ch) = self.channels.get_mut(channel) {
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
                if let Some(t) = self.mixer.tracks.get_mut(track) {
                    t.output = output.min(TRACKS - 1);
                }
            }
            Command::SetTrackVca { track, vca } => {
                if let Some(t) = self.mixer.tracks.get_mut(track) {
                    t.vca = vca.filter(|&v| v < TRACKS && v != track);
                }
            }
            Command::SetTrackSend { track, slot, target, amount } => {
                if let Some(t) = self.mixer.tracks.get_mut(track)
                    && slot < SEND_SLOTS
                {
                    t.sends[slot] = (target.min(TRACKS - 1), amount.max(0.0));
                }
            }
            Command::SetRoutingOrder(order) => {
                let old = std::mem::replace(&mut self.mixer.order, order);
                self.trash(Garbage::Routing(old));
            }
            Command::SetInsert { track, slot, mut effect } => {
                if let Some(fx) = effect.as_mut() {
                    fx.set_tempo(self.bpm as f32);
                }
                if let Some(t) = self.mixer.tracks.get_mut(track)
                    && slot < INSERT_SLOTS
                {
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
                if let Some(t) = self.mixer.tracks.get_mut(track)
                    && slot < INSERT_SLOTS
                {
                    t.sidechain[slot] = key.filter(|&k| k < TRACKS && k != track);
                }
            }
            Command::SetEffectParam { track, slot, id, value } => {
                if let Some(Some(fx)) = self.mixer.tracks.get_mut(track).and_then(|t| t.inserts.get_mut(slot)) {
                    fx.set_param(id, value);
                }
            }
            Command::SetMetronome(on) => self.metronome = on,
            Command::SetInput(input) => {
                if let Some(old) = std::mem::replace(&mut self.input, input) {
                    self.trash(Garbage::Input(old));
                }
            }
            Command::SetMonitor { track } => self.monitor = track.map(|t| t.min(TRACKS - 1)),
            Command::Record { track, armed, buffer } => {
                self.finish_recording();
                match buffer {
                    Some(mut rec) if armed => {
                        rec.len = 0;
                        rec.track = track.min(TRACKS - 1);
                        rec.sample_rate = self.sample_rate;
                        self.recording = Some(rec);
                    }
                    Some(rec) => self.trash(Garbage::Recording(rec)),
                    None => {}
                }
            }
            Command::Audition(Some(voice)) => {
                if let Some(old) = self.audition.replace(voice) {
                    self.trash(Garbage::Audition(old));
                }
            }
            Command::Audition(None) => {
                if let Some(voice) = self.audition.as_mut() {
                    voice.stop();
                }
            }
        }
        self.state.playing.store(self.playing, Ordering::Relaxed);
    }
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
    use crate::input::{InputProducer, input_channel};
    use crate::sample::Sample;
    use crate::sequence::{AudioClip, AutoPoint, AutoScope, AutomationLane, Clip, Note, Pattern, TICKS_PER_STEP};
    use std::sync::Mutex;
    use std::sync::atomic::AtomicUsize;

    type LogEntry = (&'static str, u8, u64);

    /// Instrument espion : journalise (événement, note, échantillon).
    #[derive(Clone, Default)]
    struct Probe {
        log: Arc<Mutex<Vec<LogEntry>>>,
        rendered: Arc<AtomicUsize>,
    }

    impl crate::param::Params for Probe {}

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
            .map(|&s| Note { start: s as u32 * TICKS_PER_STEP, len: TICKS_PER_STEP, key: 60, vel: 1.0 })
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

    #[test]
    fn notes_are_released_after_their_length() {
        let (mut engine, mut handle, probe) = setup(&[0]);
        handle.send(Command::Play).unwrap();
        run(&mut engine, 7_000, 512);
        assert_eq!(probe.log.lock().unwrap().clone(), [("on", 60, 0), ("off", 60, 6_000)]);
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
                [67, 60, 64].map(|key| Note { start: 0, len: 16 * TICKS_PER_STEP, key, vel: 0.9 }).to_vec();
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
            let mut edges = vec![vec![MASTER]; TRACKS];
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
    fn tempo_lane_changes_note_timing() {
        let (mut engine, mut handle, probe) = setup(&[0, 4, 8]);
        let seq = Sequence {
            patterns: vec![pattern(&[0, 4, 8])],
            clips: vec![Clip { pattern: 0, start: 0, len: 384, offset: 0 }],
            mode: PlayMode::Song,
            song_len: 384,
            automation: vec![lane(AutoTarget::Tempo, AutoScope::Song, &[(0, 120.0), (48, 120.0), (49, 240.0)])],
            ..Default::default()
        };
        handle.send(Command::SetSequence(Box::new(seq))).unwrap();
        handle.send(Command::Play).unwrap();
        run(&mut engine, 40_000, 256);
        let on = ons(&probe);
        assert_eq!(on.len(), 3, "{on:?}");
        assert_eq!(on[0], 0);
        // 48 ticks à 120 BPM (12 000 échantillons), une rampe d'un tick évaluée par blocs
        // de 256, puis le reste à 240 BPM (au lieu de 24 000 sans automation).
        assert!((17_875..=18_200).contains(&on[1]), "{on:?}");
        // Ensuite tempo constant à 240 BPM : 96 ticks × 125 échantillons.
        assert!(on[2].abs_diff(on[1] + 12_000) <= 1, "{on:?}");
        assert!((handle.position() - (192.0 + (40_000 - on[2]) as f64 / 125.0)).abs() < 0.01);
    }

    fn audio_clip(sample: Sample, start: u32, offset: f64, gain: f32) -> AudioClip {
        AudioClip { sample: Arc::new(sample), track: 2, start, len: 384 * 4, offset, gain, fade_in: 0.0, fade_out: 0.0 }
    }

    /// Moteur en mode morceau avec ces clips audio, canal sonde coupé ; rend `frames` (gauche seulement).
    fn render_audio(clips: Vec<AudioClip>, frames: usize, block: usize) -> Vec<f32> {
        let (mut engine, mut handle, _) = setup(&[]);
        let seq = Sequence { mode: PlayMode::Song, song_len: 384 * 8, audio_clips: clips, ..Default::default() };
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
        handle.send(Command::SetMonitor { track: Some(2) }).unwrap();
        assert_eq!(tx.push(&[0.5; 256], 1), 256);
        let mut buf = vec![0.0; 2 * 256];
        engine.process(&mut buf, 2);
        assert_eq!(handle.take_input_peak(), [0.5, 0.5]);
        assert_eq!(handle.take_track_peak(2), [0.5, 0.5]);
        assert_eq!(handle.take_master_peak(), [0.5, 0.5]);
        assert!(buf.iter().all(|&s| s == 0.5));

        // Sans écoute : le niveau d'entrée reste mesuré, la piste se tait.
        handle.send(Command::SetMonitor { track: None }).unwrap();
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
}
