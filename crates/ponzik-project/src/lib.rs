//! Modèle de projet Ponzik : la source de vérité de l'interface.
//!
//! Le projet décrit tout (canaux, patterns, playlist, mixer) ; il sait construire
//! un moteur complet et produire l'instantané de séquence à envoyer à chaque
//! modification.

use std::sync::Arc;

use ponzik_engine::mixer::{INSERT_SLOTS, MASTER, SEND_SLOTS, TRACKS, routing_order};
use ponzik_engine::registry;
use ponzik_engine::{
    AudioClip, AutomationLane, Channel, Clip, Command, Effect, Engine, EngineHandle, Instrument, Note, Pattern,
    PlayMode, Sample, Sequence, TICKS_PER_BAR, TICKS_PER_STEP,
};
pub use ponzik_engine::{AutoPoint, AutoScope, AutoTarget};

mod breakcore;
mod demo;
pub mod history;
pub mod io;
pub mod json;
pub mod render;

pub use breakcore::demo_breakcore;
pub use demo::demo_project;
pub use history::History;
pub use io::{load, load_from_file, save, save_to_file};
pub use render::{BitDepth, RenderMode, render_song, render_stems, write_wav};

/// Un instrument et l'état de ses paramètres.
#[derive(Clone, Debug, PartialEq)]
pub struct InstrumentDef {
    pub kind: String,
    /// Valeurs qui diffèrent des défauts : (identifiant, valeur).
    pub params: Vec<(u32, f32)>,
    /// Fichier audio chargé (sampler).
    pub sample: Option<String>,
}

impl InstrumentDef {
    pub fn new(kind: &str) -> Self {
        Self { kind: kind.into(), params: Vec::new(), sample: None }
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
    pub kind: String,
    pub params: Vec<(u32, f32)>,
    pub bypass: bool,
}

impl EffectDef {
    pub fn new(kind: &str) -> Self {
        Self { kind: kind.into(), params: Vec::new(), bypass: false }
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
    pub gain: f32,
    pub pan: f32,
    pub muted: bool,
    /// Piste de mixer.
    pub track: usize,
    /// Note posée par un clic dans le step sequencer.
    pub root_note: u8,
    pub color: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PatternDef {
    pub name: String,
    /// Longueur en ticks.
    pub len: u32,
    /// Notes par canal, triées par début.
    pub notes: Vec<Vec<Note>>,
    pub color: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClipDef {
    pub pattern: usize,
    /// Ligne de la playlist (affichage).
    pub lane: usize,
    pub start: u32,
    pub len: u32,
    pub offset: u32,
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
}

#[derive(Clone, Debug, PartialEq)]
pub struct TrackDef {
    pub name: String,
    pub gain: f32,
    pub pan: f32,
    pub muted: bool,
    pub output: usize,
    pub sends: [(usize, f32); SEND_SLOTS],
    pub inserts: [Option<EffectDef>; INSERT_SLOTS],
    pub color: u32,
}

impl TrackDef {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            gain: 1.0,
            pan: 0.0,
            muted: false,
            output: MASTER,
            sends: [(MASTER, 0.0); SEND_SLOTS],
            inserts: std::array::from_fn(|_| None),
            color: 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Project {
    pub name: String,
    pub bpm: f32,
    pub swing: f32,
    pub channels: Vec<ChannelDef>,
    pub patterns: Vec<PatternDef>,
    pub clips: Vec<ClipDef>,
    /// `TRACKS` pistes, la 0 est le master.
    pub tracks: Vec<TrackDef>,
    pub mode: PlayMode,
    pub current_pattern: usize,
    /// Boucle de lecture tirée dans une règle (non enregistrée dans le fichier).
    pub loop_range: Option<(u32, u32)>,
    pub automation: Vec<AutomationDef>,
    pub audio_clips: Vec<AudioClipDef>,
}

/// Fonction de chargement de fichiers audio (fournie par l'application).
pub type SampleLoader<'a> = &'a dyn Fn(&str) -> Option<Arc<Sample>>;

impl Default for Project {
    fn default() -> Self {
        Self::empty()
    }
}

impl Project {
    pub fn empty() -> Self {
        let mut tracks: Vec<TrackDef> = (0..TRACKS).map(|i| TrackDef::new(format!("Piste {i}"))).collect();
        tracks[MASTER].name = "Master".into();
        Self {
            name: "Sans titre".into(),
            bpm: 120.0,
            swing: 0.0,
            channels: Vec::new(),
            patterns: vec![PatternDef { name: "Pattern 1".into(), len: TICKS_PER_BAR, notes: Vec::new(), color: 0 }],
            clips: Vec::new(),
            tracks,
            mode: PlayMode::Pattern(0),
            current_pattern: 0,
            loop_range: None,
            automation: Vec::new(),
            audio_clips: Vec::new(),
        }
    }

    /// Fin du morceau : fin du dernier clip (pattern ou audio), arrondie à la mesure.
    pub fn song_len(&self) -> u32 {
        let patterns = self.clips.iter().map(|c| c.start.saturating_add(c.len));
        let audio = self.audio_clips.iter().map(|c| c.start.saturating_add(c.len));
        let end = patterns.chain(audio).max().unwrap_or(0);
        end.div_ceil(TICKS_PER_BAR).max(1).saturating_mul(TICKS_PER_BAR)
    }

    /// Instantané de séquence pour le moteur, sans les clips audio (aucun
    /// échantillon chargé) ; voir [`Project::sequence_with`].
    pub fn sequence(&self) -> Sequence {
        self.sequence_with(&|_| None)
    }

    /// Instantané de séquence complet : les clips audio dont le fichier ne se
    /// charge pas sont ignorés.
    pub fn sequence_with(&self, loader: SampleLoader) -> Sequence {
        let n = self.channels.len();
        let patterns = self
            .patterns
            .iter()
            .map(|p| {
                let mut notes = p.notes.clone();
                notes.resize(n, Vec::new());
                let mut pat = Pattern { len: p.len, notes };
                pat.sort();
                pat
            })
            .collect();
        Sequence {
            patterns,
            clips: self
                .clips
                .iter()
                .map(|c| Clip { pattern: c.pattern, start: c.start, len: c.len, offset: c.offset })
                .collect(),
            mode: self.mode,
            song_len: self.song_len(),
            swing: self.swing,
            loop_range: self.loop_range,
            automation: self
                .automation
                .iter()
                .filter(|a| !a.points.is_empty())
                .map(|a| AutomationLane { target: a.target, scope: a.scope, points: a.points.clone() })
                .collect(),
            audio_clips: self
                .audio_clips
                .iter()
                .filter_map(|c| {
                    Some(AudioClip {
                        sample: loader(&c.path)?,
                        track: c.track.min(TRACKS - 1),
                        start: c.start,
                        len: c.len,
                        offset: c.offset,
                        gain: c.gain,
                        fade_in: c.fade_in,
                        fade_out: c.fade_out,
                    })
                })
                .collect(),
        }
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
        routing_order(&edges)
    }

    /// Crée l'instrument d'un canal, réglé et prêt à envoyer au moteur.
    pub fn make_instrument(def: &InstrumentDef, sample_rate: f32, loader: SampleLoader) -> Box<dyn Instrument> {
        let mut inst = registry::create_instrument(&def.kind)
            .or_else(|| registry::create_instrument("pulse"))
            .expect("le registre contient Pulse");
        inst.set_sample_rate(sample_rate);
        for &(id, v) in &def.params {
            inst.set_param(id, v);
        }
        if let Some(path) = &def.sample
            && let Some(sample) = loader(path)
        {
            inst.set_sample(sample);
        }
        inst
    }

    pub fn make_effect(def: &EffectDef, sample_rate: f32, bpm: f32) -> Option<Box<dyn Effect>> {
        let mut fx = registry::create_effect(&def.kind)?;
        fx.set_sample_rate(sample_rate);
        fx.set_tempo(bpm);
        for &(id, v) in &def.params {
            fx.set_param(id, v);
        }
        Some(fx)
    }

    pub fn make_channel(&self, idx: usize, sample_rate: f32, loader: SampleLoader) -> Channel {
        let c = &self.channels[idx];
        let mut ch = Channel::new(c.name.clone(), Self::make_instrument(&c.instrument, sample_rate, loader))
            .with_gain(c.gain)
            .with_pan(c.pan)
            .with_track(c.track);
        ch.muted = c.muted;
        ch
    }

    /// Moteur complet dans l'état du projet.
    pub fn build_engine(&self, sample_rate: f32, loader: SampleLoader) -> (Engine, EngineHandle) {
        let channels = (0..self.channels.len()).map(|i| self.make_channel(i, sample_rate, loader)).collect();
        let (mut engine, mut handle) = Engine::new(sample_rate, channels, self.sequence_with(loader));
        let mixer = engine.mixer_mut();
        for (i, def) in self.tracks.iter().enumerate().take(TRACKS) {
            let t = &mut mixer.tracks[i];
            t.gain = def.gain;
            t.pan = def.pan;
            t.muted = def.muted;
            t.output = def.output;
            t.sends = def.sends;
            for (slot, fx) in def.inserts.iter().enumerate() {
                t.inserts[slot] = fx.as_ref().and_then(|d| Self::make_effect(d, sample_rate, self.bpm));
                t.bypass[slot] = fx.as_ref().is_some_and(|d| d.bypass);
            }
        }
        if let Some(order) = self.routing_order() {
            mixer.order = order;
        }
        let _ = handle.send(Command::SetBpm(self.bpm));
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
                notes.insert(at, Note { start, len: TICKS_PER_STEP, key, vel: 0.8 });
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

    /// Ajoute un canal et sa piste de mixer libre suivante.
    pub fn add_channel(&mut self, name: impl Into<String>, instrument: InstrumentDef, root_note: u8) -> usize {
        let used: Vec<usize> = self.channels.iter().map(|c| c.track).collect();
        let track = (1..TRACKS).find(|t| !used.contains(t)).unwrap_or(1);
        let name = name.into();
        if self.tracks[track].name.starts_with("Piste ") {
            self.tracks[track].name = name.clone();
        }
        self.channels.push(ChannelDef {
            name,
            instrument,
            gain: 0.8,
            pan: 0.0,
            muted: false,
            track,
            root_note,
            color: 0,
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
                | AutoTarget::ChannelPan(channel) => channel,
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

    pub fn add_pattern(&mut self) -> usize {
        let n = self.patterns.len() + 1;
        self.patterns.push(PatternDef {
            name: format!("Pattern {n}"),
            len: TICKS_PER_BAR,
            notes: vec![Vec::new(); self.channels.len()],
            color: 0,
        });
        self.patterns.len() - 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_samples(_: &str) -> Option<Arc<Sample>> {
        None
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
        p.clips.push(ClipDef { pattern: 0, lane: 0, start: TICKS_PER_BAR * 2, len: TICKS_PER_BAR / 2, offset: 0 });
        assert_eq!(p.song_len(), TICKS_PER_BAR * 3);
        assert!(p.routing_order().is_some());
        p.tracks[1].output = 2;
        p.tracks[2].output = 1;
        assert!(p.routing_order().is_none());
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
        }
    }

    #[test]
    fn audio_clips_extend_the_song_and_need_their_sample() {
        let mut p = Project::empty();
        p.clips.push(ClipDef { pattern: 0, lane: 0, start: 0, len: TICKS_PER_BAR, offset: 0 });
        p.audio_clips.push(audio("a.wav", TICKS_PER_BAR * 4, 10));
        p.audio_clips.push(audio("absent.wav", 0, 10));
        assert_eq!(p.song_len(), TICKS_PER_BAR * 5);
        assert!(p.sequence().audio_clips.is_empty());
        let sample =
            Arc::new(Sample { name: "a".into(), sample_rate: 48_000.0, left: vec![0.5; 4], right: vec![0.5; 4] });
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
}
