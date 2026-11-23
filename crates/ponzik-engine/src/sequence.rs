//! Données de séquence : notes, patterns, clips de playlist, automation et clips
//! audio. Le moteur en reçoit un instantané complet à chaque modification et y
//! cherche les notes à jouer.

use std::sync::Arc;

use crate::sample::Sample;
use crate::tempo_map::{TempoMap, TempoView};

/// Résolution : ticks par noire.
pub const PPQ: u32 = 96;
/// Une double-croche, le pas du step sequencer.
pub const TICKS_PER_STEP: u32 = PPQ / 4;
/// Une mesure en 4/4 (grilles de pas des patterns ; la playlist suit la
/// carte des signatures, voir [`TempoMap`]).
pub const TICKS_PER_BAR: u32 = PPQ * 4;

/// Most repeats a note can ratchet into.
pub const MAX_RATCHET: u8 = 8;

/// When a note plays, counted in passes of its pattern (a pattern looping, or
/// each repeat inside a playlist clip or a session slot), like Elektron's trig
/// conditions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum NoteCond {
    #[default]
    Always,
    /// The first pass only.
    First,
    /// Every pass but the first.
    NotFirst,
    /// Pass `n` (from 1) of every `m`: `Every(1, 2)` plays one pass out of two,
    /// `Every(3, 4)` the third of every four.
    Every(u8, u8),
}

impl NoteCond {
    /// True if the note plays on pass `pass` (0 is the first).
    pub fn holds(self, pass: i64) -> bool {
        match self {
            Self::Always => true,
            Self::First => pass == 0,
            Self::NotFirst => pass != 0,
            Self::Every(n, m) => {
                let m = i64::from(m.max(1));
                pass.rem_euclid(m) == (i64::from(n.max(1)) - 1) % m
            }
        }
    }

    /// Short label, also the project file id: "1:2", "first", "!first", "" for always.
    pub fn id(self) -> String {
        match self {
            Self::Always => String::new(),
            Self::First => "first".into(),
            Self::NotFirst => "!first".into(),
            Self::Every(n, m) => format!("{n}:{m}"),
        }
    }

    /// Inverse of [`NoteCond::id`]; `None` for an unknown or out of range condition.
    pub fn from_id(s: &str) -> Option<Self> {
        match s.trim() {
            "" | "always" => Some(Self::Always),
            "first" => Some(Self::First),
            "!first" => Some(Self::NotFirst),
            s => {
                let (n, m) = s.split_once(':')?;
                let (n, m) = (n.trim().parse::<u8>().ok()?, m.trim().parse::<u8>().ok()?);
                ((1..=16).contains(&m) && (1..=m).contains(&n)).then_some(Self::Every(n, m))
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Note {
    /// Début en ticks depuis le début du pattern.
    pub start: u32,
    /// Durée en ticks.
    pub len: u32,
    pub key: u8,
    /// Dans [0, 1].
    pub vel: f32,
    /// Chance to play on each pass, in percent (100: always).
    pub prob: u8,
    /// Passes on which the note plays.
    pub cond: NoteCond,
    /// Repeats played inside the note's length, 1 to [`MAX_RATCHET`].
    pub ratchet: u8,
    /// Slide note (FL Studio): plays nothing by itself, it glides the notes
    /// held on its channel to its key over its length.
    pub slide: bool,
    /// Pan of the note in [−1, 1] (0: center).
    pub pan: f32,
    /// Fine pitch in cents, [−100, 100].
    pub fine: f32,
}

/// Fine pitch range of a note, in cents (±).
pub const MAX_FINE: f32 = 100.0;

impl Note {
    /// A note that always plays, once.
    pub const DEFAULT: Note = Note {
        start: 0,
        len: TICKS_PER_STEP,
        key: 60,
        vel: 0.8,
        prob: 100,
        cond: NoteCond::Always,
        ratchet: 1,
        slide: false,
        pan: 0.0,
        fine: 0.0,
    };

    pub const fn new(start: u32, len: u32, key: u8, vel: f32) -> Self {
        Self { start, len, key, vel, ..Self::DEFAULT }
    }

    /// True when the note has a pan or a fine pitch.
    pub fn has_expression(&self) -> bool {
        self.pan != 0.0 || self.fine != 0.0
    }

    /// Pitch of the note in semitones, fine pitch included.
    pub fn pitch(&self) -> f32 {
        f32::from(self.key) + self.fine / 100.0
    }

    /// True when the note has a chance, a condition or repeats.
    pub fn is_varied(&self) -> bool {
        self.prob < 100 || self.cond != NoteCond::Always || self.ratchet > 1
    }

    /// Ticks over which the repeats spread: the note's length, at most a bar.
    pub fn ratchet_span(&self) -> u32 {
        self.len.clamp(1, TICKS_PER_BAR)
    }

    /// True if the note plays on pass `pass` of pattern `pattern`, channel
    /// `channel`: its condition holds and it wins its draw. The draw is a hash
    /// of where the note is, so a given pass always plays the same way.
    pub fn plays(&self, pattern: usize, channel: usize, pass: i64) -> bool {
        if !self.cond.holds(pass) {
            return false;
        }
        if self.prob >= 100 {
            return true;
        }
        let seed = (pattern as u64) << 48 ^ (channel as u64) << 32 ^ u64::from(self.start) << 8 ^ u64::from(self.key);
        let draw = splitmix(seed ^ (pass as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15)) % 100;
        draw < u64::from(self.prob)
    }
}

impl Default for Note {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// SplitMix64 finalizer: a well-mixed hash of `x`.
pub fn splitmix(x: u64) -> u64 {
    let mut z = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// Un pattern contient les notes de tous les canaux, comme dans FL Studio.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pattern {
    /// Longueur en ticks (la boucle du pattern).
    pub len: u32,
    /// Notes par canal, triées par début.
    pub notes: Vec<Vec<Note>>,
    /// Longest span of a ratcheted note (set by [`Pattern::sort`]): how far
    /// back the sequencer looks for repeats that land in a window.
    pub ratchet_span: u32,
}

impl Pattern {
    pub fn new(len: u32, channels: usize) -> Self {
        Self { len, notes: vec![Vec::new(); channels], ratchet_span: 0 }
    }

    /// Sorts the notes by start and measures the ratchets: call it after
    /// changing the notes.
    pub fn sort(&mut self) {
        for notes in &mut self.notes {
            notes.sort_by_key(|n| (n.start, n.key));
        }
        self.ratchet_span = self.notes.iter().flatten().filter(|n| n.ratchet > 1).map(Note::ratchet_span).max().unwrap_or(0);
    }
}

/// Occurrence d'un pattern dans la playlist.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Clip {
    pub pattern: usize,
    pub start: u32,
    pub len: u32,
    /// Décalage dans le pattern au début du clip (clip rogné à gauche).
    pub offset: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayMode {
    /// Boucle sur un pattern (mode pattern de FL Studio).
    Pattern(usize),
    /// Joue la playlist.
    Song,
    /// Vue Session : les clips lancés bouclent chacun depuis leur départ.
    Session,
}

/// Clip lancé en vue Session : un pattern qui boucle de `start` à `end` (ticks),
/// `end` = `u32::MAX` tant qu'il n'est pas remplacé ou arrêté.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SessionSlot {
    pub pattern: usize,
    pub start: u32,
    pub end: u32,
    /// Ticks into the pattern at `start` (legato launch: the position of the
    /// clip it replaces).
    pub offset: u32,
    /// Non-zero: the clip starts over every `period` ticks (repeat launch).
    pub period: u32,
}

impl SessionSlot {
    /// Position in the pattern (ticks from its start, before looping) at `tick`.
    pub fn local(&self, tick: f64) -> f64 {
        let since = tick - self.start as f64;
        let since = if self.period > 0 { since.rem_euclid(self.period as f64) } else { since };
        since + self.offset as f64
    }
}

/// Ce que pilote une courbe d'automation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutoTarget {
    InstrumentParam { channel: usize, id: u32 },
    EffectParam { track: usize, slot: usize, id: u32 },
    ChannelGain(usize),
    ChannelPan(usize),
    TrackGain(usize),
    TrackPan(usize),
    /// En BPM.
    Tempo,
    /// Pitch bend of a channel's instrument, in [−1, 1].
    PitchBend(usize),
    /// Control change `cc` (1: the mod wheel) sent to a channel's
    /// instrument, in [0, 1].
    Cc { channel: usize, cc: u8 },
}

impl AutoTarget {
    /// Sent to the instrument as events on their exact sample (each tick of
    /// a ramp), rather than once per block.
    pub fn is_event(self) -> bool {
        matches!(self, Self::InstrumentParam { .. } | Self::PitchBend(_) | Self::Cc { .. })
    }

    /// The channel whose instrument the target drives, if any.
    pub fn channel(self) -> Option<usize> {
        match self {
            Self::InstrumentParam { channel, .. } | Self::Cc { channel, .. } | Self::PitchBend(channel) => Some(channel),
            Self::ChannelGain(c) | Self::ChannelPan(c) => Some(c),
            _ => None,
        }
    }
}

/// Portée d'une courbe d'automation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutoScope {
    /// Sur la ligne de temps du morceau, en mode morceau seulement.
    Song,
    /// Relative au pattern : boucle avec lui en mode pattern et suit chacun de
    /// ses clips en mode morceau (automation de pattern façon FL Studio).
    Pattern(usize),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AutoPoint {
    pub tick: u32,
    /// En unités réelles du paramètre.
    pub value: f32,
    /// Tension du segment qui part de ce point, dans [-1, 1] ; 0 = linéaire.
    pub curve: f32,
}

/// Courbe d'automation : points triés par tick, valeur tenue avant le premier
/// et après le dernier.
#[derive(Clone, Debug, PartialEq)]
pub struct AutomationLane {
    pub target: AutoTarget,
    pub scope: AutoScope,
    pub points: Vec<AutoPoint>,
}

impl AutomationLane {
    pub fn value_at(&self, tick: f64) -> Option<f32> {
        automation_value(&self.points, tick)
    }
}

/// Mise en forme d'un segment : `t` dans [0, 1], tension dans [-1, 1].
/// Tension positive : départ lent (courbe creusée) ; négative : départ rapide.
pub fn shape_curve(t: f64, curve: f32) -> f64 {
    let t = t.clamp(0.0, 1.0);
    let c = curve.clamp(-1.0, 1.0) as f64;
    if c.abs() < 1e-6 {
        return t;
    }
    // Courbe de puissance, exposant de 1/5 à 5.
    t.powf(5f64.powf(c))
}

/// Valeur d'une suite de points triés à `tick` ; `None` sans point.
pub fn automation_value(points: &[AutoPoint], tick: f64) -> Option<f32> {
    let first = points.first()?;
    let next = points.partition_point(|p| p.tick as f64 <= tick);
    if next == 0 {
        return Some(first.value);
    }
    let a = points[next - 1];
    let Some(b) = points.get(next) else { return Some(a.value) };
    let span = b.tick as f64 - a.tick as f64;
    if span <= 0.0 {
        return Some(b.value);
    }
    let t = shape_curve((tick - a.tick as f64) / span, a.curve);
    Some((a.value as f64 + (b.value as f64 - a.value as f64) * t) as f32)
}

/// Audio file placed in the playlist. Unwarped, it plays at its own speed;
/// warped, its beats follow the song's tempo map.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AudioClip {
    pub sample: Arc<Sample>,
    /// Piste de mixer de destination.
    pub track: usize,
    /// Début en ticks.
    pub start: u32,
    /// Durée en ticks.
    pub len: u32,
    /// Décalage dans l'échantillon au début du clip, en secondes.
    pub offset: f64,
    pub gain: f32,
    /// Fondus en secondes (au moins 2 ms, contre les clics). Negative: no
    /// fade at all (offline renders whose edges are faded later).
    pub fade_in: f32,
    pub fade_out: f32,
    /// Fade shapes in [−1, 1]: 0 is linear, positive starts slowly, negative quickly.
    pub fade_in_curve: f32,
    pub fade_out_curve: f32,
    /// Equal-power crossfades in seconds, centered on the start and the end
    /// (0: none, the fades above apply). The clip starts sounding half a
    /// crossfade before its start and stops half a crossfade after its end,
    /// so two clips meeting on a tick with the same crossfade sum to constant
    /// power: comp segments of a take group.
    pub xfade_in: f32,
    pub xfade_out: f32,
    /// The file is played backwards (offset and warp markers count in the reversed file).
    pub reverse: bool,
    /// Past its end, the file starts over.
    pub looped: bool,
    pub warp: Option<ClipWarp>,
    /// Non-zero (session clips): the clip starts over every `cycle` ticks,
    /// fades included; a warped clip's render then covers one cycle.
    pub cycle: u32,
    /// Ticks into the cycle at `start` (legato launch).
    pub phase: u32,
}

/// Warping of a clip: its beat map, and the stretched audio once rendered
/// off the audio thread. Until then the clip plays resampled (its pitch
/// follows the tempo), always in time.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClipWarp {
    pub map: WarpMap,
    /// Clip audio at the engine rate from the clip start, for the song's tempo map.
    pub rendered: Option<Arc<Sample>>,
}

/// A warp marker: a point of the file (seconds) pinned to a beat.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WarpMarker {
    pub secs: f64,
    pub beat: f64,
}

/// Beats of a file: its original tempo, bent by warp markers. Beat 0 is the
/// start of the file; outside the markers, beats go by at `bpm`.
#[derive(Clone, Debug, PartialEq)]
pub struct WarpMap {
    pub bpm: f64,
    /// Increasing in both seconds and beats.
    markers: Vec<WarpMarker>,
}

impl Default for WarpMap {
    fn default() -> Self {
        Self::new(120.0, Vec::new())
    }
}

impl WarpMap {
    /// Markers are sorted; those that would run backwards are dropped.
    pub fn new(bpm: f64, mut markers: Vec<WarpMarker>) -> Self {
        let bpm = if bpm.is_finite() && bpm > 0.0 { bpm.clamp(10.0, 999.0) } else { 120.0 };
        markers.retain(|m| m.secs.is_finite() && m.beat.is_finite());
        markers.sort_by(|a, b| a.secs.total_cmp(&b.secs));
        let mut kept: Vec<WarpMarker> = Vec::with_capacity(markers.len());
        for m in markers {
            if kept.last().is_none_or(|l| m.secs > l.secs + 1e-6 && m.beat > l.beat + 1e-6) {
                kept.push(m);
            }
        }
        Self { bpm, markers: kept }
    }

    pub fn markers(&self) -> &[WarpMarker] {
        &self.markers
    }

    /// Beat of a point of the file.
    pub fn beat_at(&self, secs: f64) -> f64 {
        let k = self.bpm / 60.0;
        let m = &self.markers;
        let (Some(first), Some(last)) = (m.first(), m.last()) else { return secs * k };
        if secs <= first.secs {
            return first.beat + (secs - first.secs) * k;
        }
        if secs >= last.secs {
            return last.beat + (secs - last.secs) * k;
        }
        let i = m.partition_point(|p| p.secs <= secs);
        let (a, b) = (m[i - 1], m[i]);
        a.beat + (secs - a.secs) * (b.beat - a.beat) / (b.secs - a.secs)
    }

    /// Point of the file at a beat (inverse of [`WarpMap::beat_at`]).
    pub fn secs_at(&self, beat: f64) -> f64 {
        let k = 60.0 / self.bpm;
        let m = &self.markers;
        let (Some(first), Some(last)) = (m.first(), m.last()) else { return beat * k };
        if beat <= first.beat {
            return first.secs + (beat - first.beat) * k;
        }
        if beat >= last.beat {
            return last.secs + (beat - last.beat) * k;
        }
        let i = m.partition_point(|p| p.beat <= beat);
        let (a, b) = (m[i - 1], m[i]);
        a.secs + (beat - a.beat) * (b.secs - a.secs) / (b.beat - a.beat)
    }
}

/// Gain of an equal-power fade in at `x` in [0, 1]: the fade out over the
/// same span is `equal_power(1 − x)`, and the squares of both sum to 1.
#[inline]
pub fn equal_power(x: f64) -> f64 {
    (x.clamp(0.0, 1.0) * std::f64::consts::FRAC_PI_2).sin()
}

/// Fade gain for a position `x` in [0, 1] of the fade, bent by `curve`.
#[inline]
pub fn fade_shape(x: f64, curve: f32) -> f64 {
    let x = x.clamp(0.0, 1.0);
    if curve.abs() < 1e-3 { x } else { x.powf((2.0 * curve as f64).exp2()) }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Sequence {
    pub patterns: Vec<Pattern>,
    pub clips: Vec<Clip>,
    pub mode: PlayMode,
    /// Fin de la playlist en ticks (le morceau boucle ensuite).
    pub song_len: u32,
    /// Retard des doubles-croches paires, dans [0, 1] (1 = un demi-pas).
    pub swing: f32,
    pub automation: Vec<AutomationLane>,
    /// Played in song mode (the playlist's clips) and in session mode (the
    /// launched audio clips), never in pattern mode.
    pub audio_clips: Vec<AudioClip>,
    /// Boucle de lecture (début, fin) en ticks, tirée dans une règle.
    pub loop_range: Option<(u32, u32)>,
    /// Clips lancés en vue Session.
    pub session: Vec<SessionSlot>,
    /// Tempo and time signature changes of the song timeline. Patterns and
    /// session clips play at the first tempo and signature.
    pub tempo: TempoMap,
}

impl Default for Sequence {
    fn default() -> Self {
        Self {
            patterns: Vec::new(),
            clips: Vec::new(),
            mode: PlayMode::Pattern(0),
            song_len: TICKS_PER_BAR,
            swing: 0.0,
            automation: Vec::new(),
            audio_clips: Vec::new(),
            loop_range: None,
            session: Vec::new(),
            tempo: TempoMap::default(),
        }
    }
}

/// Note à déclencher, en position absolue.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SeqEvent {
    pub tick: f64,
    pub channel: usize,
    pub key: u8,
    pub vel: f32,
    /// In ticks (a ratchet repeat can last a fraction of a tick).
    pub len: f64,
    /// A slide note: glides the held notes instead of playing.
    pub slide: bool,
    /// Pan in [−1, 1].
    pub pan: f32,
    /// Fine pitch in cents.
    pub fine: f32,
}

impl Sequence {
    /// Tempo map in effect: the whole map in song mode, its first tempo and
    /// signature otherwise.
    pub fn tempo_view(&self) -> TempoView<'_> {
        match self.mode {
            PlayMode::Song => self.tempo.view(),
            PlayMode::Pattern(_) | PlayMode::Session => self.tempo.head(),
        }
    }

    /// Boucle explicite valide (non vide).
    fn active_loop(&self) -> Option<(u32, u32)> {
        self.loop_range.filter(|(a, b)| b > a)
    }

    /// Début de la boucle du transport.
    pub fn loop_start(&self) -> f64 {
        self.active_loop().map_or(0.0, |(a, _)| a as f64)
    }

    /// Fin de boucle du transport : la boucle tirée dans la règle s'il y en a une,
    /// sinon la playlist boucle et un pattern est infini.
    pub fn loop_end(&self) -> Option<f64> {
        if let Some((_, b)) = self.active_loop() {
            return Some(b as f64);
        }
        match self.mode {
            PlayMode::Song => Some(self.song_len.max(TICKS_PER_BAR) as f64),
            PlayMode::Pattern(_) | PlayMode::Session => None,
        }
    }

    /// Longueur affichée de la boucle courante, en ticks.
    pub fn cycle_len(&self) -> u32 {
        match self.mode {
            PlayMode::Song => self.song_len.max(TICKS_PER_BAR),
            PlayMode::Pattern(p) => self.patterns.get(p).map_or(TICKS_PER_BAR, |p| p.len.max(1)),
            PlayMode::Session => TICKS_PER_BAR,
        }
    }

    /// Position locale d'une courbe de portée `scope` quand le transport est à
    /// `pos`, ou `None` si elle ne s'applique pas ici. En mode morceau, une
    /// courbe de pattern suit le dernier clip de ce pattern qui couvre `pos`.
    pub fn automation_tick(&self, scope: AutoScope, pos: f64) -> Option<f64> {
        let pattern_len = |p: usize| Some(self.patterns.get(p)?.len.max(1) as f64);
        match (scope, self.mode) {
            (AutoScope::Song, PlayMode::Song) => Some(pos),
            (AutoScope::Song, PlayMode::Pattern(_) | PlayMode::Session) => None,
            (AutoScope::Pattern(p), PlayMode::Session) => {
                let slot = self.session.iter().rfind(|s| s.pattern == p && pos >= s.start as f64 && pos < s.end as f64)?;
                Some(slot.local(pos).rem_euclid(pattern_len(p)?))
            }
            (AutoScope::Pattern(p), PlayMode::Pattern(cur)) if p == cur => Some(pos.rem_euclid(pattern_len(p)?)),
            (AutoScope::Pattern(_), PlayMode::Pattern(_)) => None,
            (AutoScope::Pattern(p), PlayMode::Song) => {
                let len = pattern_len(p)?;
                let clip = self
                    .clips
                    .iter()
                    .rfind(|c| c.pattern == p && pos >= c.start as f64 && pos < c.start as f64 + c.len as f64)?;
                Some((pos - clip.start as f64 + clip.offset as f64).rem_euclid(len))
            }
        }
    }

    fn swing_offset(&self, start: u32) -> f64 {
        if start % (2 * TICKS_PER_STEP) == TICKS_PER_STEP {
            self.swing.clamp(0.0, 1.0) as f64 * TICKS_PER_STEP as f64 * 0.5
        } else {
            0.0
        }
    }

    /// Remplit `out` avec les notes qui commencent dans [`from`, `to`), triées.
    pub fn collect(&self, from: f64, to: f64, out: &mut Vec<SeqEvent>) {
        out.clear();
        match self.mode {
            PlayMode::Pattern(p) => {
                if let Some(pat) = self.patterns.get(p) {
                    if self.active_loop().is_some() {
                        // A time selection plays what it covers, once per lap:
                        // past the pattern's end it is silent, never bar 1 again.
                        let end = to.min(pat.len.max(1) as f64);
                        if from < end {
                            self.collect_looped(p, pat, 0.0, from, end, out);
                        }
                    } else {
                        self.collect_looped(p, pat, 0.0, from, to, out);
                    }
                }
            }
            PlayMode::Song => {
                for clip in &self.clips {
                    let Some(pat) = self.patterns.get(clip.pattern) else { continue };
                    let a = from.max(clip.start as f64);
                    let b = to.min((clip.start + clip.len) as f64);
                    if a < b {
                        self.collect_looped(clip.pattern, pat, clip.start as f64 - clip.offset as f64, a, b, out);
                    }
                }
            }
            PlayMode::Session => {
                for slot in &self.session {
                    let Some(pat) = self.patterns.get(slot.pattern) else { continue };
                    let a = from.max(slot.start as f64);
                    let b = to.min(slot.end as f64);
                    if a >= b {
                        continue;
                    }
                    if slot.period == 0 {
                        self.collect_looped(slot.pattern, pat, slot.start as f64 - slot.offset as f64, a, b, out);
                        continue;
                    }
                    // Repeat: each period plays the pattern again from the offset.
                    let period = slot.period as f64;
                    let mut base = slot.start as f64 + ((a - slot.start as f64) / period).floor() * period;
                    while base < b {
                        let (x, y) = (a.max(base), b.min(base + period));
                        if x < y {
                            self.collect_looped(slot.pattern, pat, base - slot.offset as f64, x, y, out);
                        }
                        base += period;
                    }
                }
            }
        }
        out.sort_unstable_by(|a, b| a.tick.total_cmp(&b.tick).then(a.channel.cmp(&b.channel)));
    }

    /// Notes d'un pattern répété à partir de `origin`, dans [`from`, `to`).
    /// Each pass of pattern `index` draws its chances and checks its
    /// conditions; a ratcheted note fires its repeats evenly over its span.
    fn collect_looped(&self, index: usize, pat: &Pattern, origin: f64, from: f64, to: f64, out: &mut Vec<SeqEvent>) {
        let len = pat.len.max(1) as f64;
        let max_swing = TICKS_PER_STEP as f64 * 0.5;
        // Un cycle plus tôt : le swing peut faire déborder une note dans la fenêtre.
        let mut base = origin + ((from - origin) / len).floor() * len - len;
        while base < to {
            let pass = ((base - origin) / len).round() as i64;
            let lo = (from - base - max_swing - pat.ratchet_span as f64).max(0.0);
            let hi = to - base;
            for (channel, notes) in pat.notes.iter().enumerate() {
                let first = notes.partition_point(|n| (n.start as f64) < lo);
                for n in &notes[first..] {
                    if n.start as f64 >= hi || n.start >= pat.len {
                        break;
                    }
                    let tick = base + n.start as f64 + self.swing_offset(n.start);
                    let repeats = if n.slide { 1 } else { n.ratchet.clamp(1, MAX_RATCHET) };
                    let (step, sub_len) = if repeats > 1 {
                        let step = n.ratchet_span() as f64 / f64::from(repeats);
                        (step, step)
                    } else {
                        (0.0, n.len.max(1) as f64)
                    };
                    let last = tick + step * f64::from(repeats - 1);
                    if last < from || tick >= to || !n.plays(index, channel, pass) {
                        continue;
                    }
                    for k in 0..repeats {
                        let t = tick + step * f64::from(k);
                        if t >= from && t < to && out.len() < out.capacity().max(1) {
                            out.push(SeqEvent { tick: t, channel, key: n.key, vel: n.vel, len: sub_len, slide: n.slide, pan: n.pan, fine: n.fine });
                        }
                    }
                }
            }
            base += len;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(start: u32) -> Note {
        Note::new(start, TICKS_PER_STEP, 60, 1.0)
    }

    fn pattern(starts: &[u32], len: u32) -> Pattern {
        let mut p = Pattern::new(len, 1);
        p.notes[0] = starts.iter().map(|&s| note(s)).collect();
        p
    }

    fn ticks(seq: &Sequence, from: f64, to: f64) -> Vec<f64> {
        let mut out = Vec::with_capacity(64);
        seq.collect(from, to, &mut out);
        out.iter().map(|e| e.tick).collect()
    }

    #[test]
    fn pattern_mode_loops() {
        let seq = Sequence { patterns: vec![pattern(&[0, 96], 192)], ..Default::default() };
        assert_eq!(ticks(&seq, 0.0, 192.0), [0.0, 96.0]);
        assert_eq!(ticks(&seq, 150.0, 300.0), [192.0, 288.0]);
        assert_eq!(ticks(&seq, 96.0, 96.5), [96.0]);
        assert!(ticks(&seq, 96.5, 191.0).is_empty());
    }

    #[test]
    fn song_mode_clips_loop_and_offset() {
        let seq = Sequence {
            patterns: vec![pattern(&[0, 48], 96)],
            clips: vec![
                Clip { pattern: 0, start: 0, len: 192, offset: 0 },
                Clip { pattern: 0, start: 384, len: 48, offset: 48 },
            ],
            mode: PlayMode::Song,
            song_len: 768,
            ..Default::default()
        };
        assert_eq!(ticks(&seq, 0.0, 768.0), [0.0, 48.0, 96.0, 144.0, 384.0]);
        assert_eq!(seq.loop_end(), Some(768.0));
    }

    #[test]
    fn session_slots_loop_from_their_start() {
        let seq = Sequence {
            patterns: vec![pattern(&[0], 96), pattern(&[48], 96)],
            mode: PlayMode::Session,
            session: vec![
                SessionSlot { pattern: 0, start: 0, end: 288, ..Default::default() },
                SessionSlot { pattern: 1, start: 192, end: u32::MAX, ..Default::default() },
            ],
            ..Default::default()
        };
        assert_eq!(ticks(&seq, 0.0, 384.0), [0.0, 96.0, 192.0, 240.0, 336.0]);
        assert_eq!(seq.loop_end(), None);
    }

    #[test]
    fn session_slots_start_at_their_offset_and_repeat() {
        // Notes every 24 ticks over 96: the legato clip starts 48 ticks in.
        let pat = pattern(&[0, 24, 48, 72], 96);
        let legato = SessionSlot { pattern: 0, start: 384, end: 480, offset: 48, period: 0 };
        let seq = Sequence { patterns: vec![pat.clone()], mode: PlayMode::Session, session: vec![legato], ..Default::default() };
        assert_eq!(ticks(&seq, 0.0, 1000.0), [384.0, 408.0, 432.0, 456.0]);
        assert_eq!(seq.automation_tick(AutoScope::Pattern(0), 400.0), Some(64.0));
        // Repeat every 36 ticks: the first two notes, again and again.
        let repeat = SessionSlot { pattern: 0, start: 0, end: 108, offset: 0, period: 36 };
        let seq = Sequence { patterns: vec![pat], mode: PlayMode::Session, session: vec![repeat], ..Default::default() };
        assert_eq!(ticks(&seq, 0.0, 1000.0), [0.0, 24.0, 36.0, 60.0, 72.0, 96.0]);
        // Window by window, the same notes.
        let mut by_block = Vec::new();
        for k in 0..100 {
            by_block.extend(ticks(&seq, k as f64 * 10.0, (k + 1) as f64 * 10.0));
        }
        assert_eq!(by_block, [0.0, 24.0, 36.0, 60.0, 72.0, 96.0]);
        assert_eq!(seq.automation_tick(AutoScope::Pattern(0), 40.0), Some(4.0));
    }

    #[test]
    fn conditions_count_passes() {
        let holds = |c: NoteCond| (0..8).map(|p| c.holds(p)).collect::<Vec<_>>();
        let (t, f) = (true, false);
        assert_eq!(holds(NoteCond::Always), [t; 8]);
        assert_eq!(holds(NoteCond::First), [t, f, f, f, f, f, f, f]);
        assert_eq!(holds(NoteCond::NotFirst), [f, t, t, t, t, t, t, t]);
        assert_eq!(holds(NoteCond::Every(1, 2)), [t, f, t, f, t, f, t, f]);
        assert_eq!(holds(NoteCond::Every(2, 2)), [f, t, f, t, f, t, f, t]);
        assert_eq!(holds(NoteCond::Every(3, 4)), [f, f, t, f, f, f, t, f]);
        for c in [NoteCond::Always, NoteCond::First, NoteCond::NotFirst, NoteCond::Every(3, 4), NoteCond::Every(8, 8)] {
            assert_eq!(NoteCond::from_id(&c.id()), Some(c));
        }
        assert_eq!(NoteCond::from_id("5:4"), None);
        assert_eq!(NoteCond::from_id("0:4"), None);
        assert_eq!(NoteCond::from_id("x"), None);
    }

    fn seq_of(notes: Vec<Note>, len: u32) -> Sequence {
        let mut p = Pattern::new(len, 1);
        p.notes[0] = notes;
        p.sort();
        Sequence { patterns: vec![p], ..Default::default() }
    }

    #[test]
    fn conditions_play_on_their_passes() {
        let n = Note { cond: NoteCond::Every(2, 3), ..note(24) };
        let seq = seq_of(vec![note(0), n], 96);
        // Passes 0..6: the conditioned note plays on passes 1 and 4.
        assert_eq!(ticks(&seq, 0.0, 576.0), [0.0, 96.0, 120.0, 192.0, 288.0, 384.0, 408.0, 480.0]);
        // Counted from the clip start in song mode.
        let first = Note { cond: NoteCond::First, ..note(0) };
        let mut seq = seq_of(vec![first], 96);
        seq.mode = PlayMode::Song;
        seq.song_len = 1_000;
        seq.clips = vec![Clip { pattern: 0, start: 192, len: 288, offset: 0 }, Clip { pattern: 0, start: 576, len: 96, offset: 0 }];
        assert_eq!(ticks(&seq, 0.0, 1_000.0), [192.0, 576.0]);
    }

    #[test]
    fn probability_is_deterministic_per_pass() {
        let half = Note { prob: 50, ..note(0) };
        let seq = seq_of(vec![half], 96);
        let mut out = Vec::with_capacity(1_000);
        seq.collect(0.0, 96.0 * 400.0, &mut out);
        let all: Vec<f64> = out.iter().map(|e| e.tick).collect();
        // Same passes on a second listen, in any window size.
        let mut pieces = Vec::new();
        for k in 0..800 {
            pieces.extend(ticks(&seq, k as f64 * 48.0, (k + 1) as f64 * 48.0));
        }
        assert_eq!(all, pieces);
        assert!((150..250).contains(&all.len()), "{}", all.len());
        let never = seq_of(vec![Note { prob: 0, ..note(0) }], 96);
        assert!(ticks(&never, 0.0, 9_600.0).is_empty());
        let always = seq_of(vec![Note { prob: 100, ..note(0) }], 96);
        assert_eq!(ticks(&always, 0.0, 6_000.0).len(), 63);
    }

    #[test]
    fn ratchets_split_the_note() {
        let n = Note { ratchet: 4, ..note(24) };
        let seq = seq_of(vec![n], 96);
        let mut out = Vec::with_capacity(16);
        seq.collect(0.0, 96.0, &mut out);
        assert_eq!(out.iter().map(|e| e.tick).collect::<Vec<_>>(), [24.0, 30.0, 36.0, 42.0]);
        assert!(out.iter().all(|e| e.len == 6.0));
        // Repeats still found when the window starts after the note.
        assert_eq!(ticks(&seq, 33.0, 40.0), [36.0]);
        assert_eq!(ticks(&seq, 41.0, 121.0), [42.0, 120.0]);
        // An odd count over a step: fractional ticks.
        let seq = seq_of(vec![Note { ratchet: 3, ..note(0) }], 96);
        assert_eq!(ticks(&seq, 0.0, 96.0), [0.0, 8.0, 16.0]);
    }

    #[test]
    fn swing_delays_even_sixteenths() {
        let seq = Sequence { patterns: vec![pattern(&[0, 24, 48], 96)], swing: 1.0, ..Default::default() };
        assert_eq!(ticks(&seq, 0.0, 96.0), [0.0, 36.0, 48.0]);
        // La note décalée reste trouvée même si la fenêtre commence après son pas.
        assert_eq!(ticks(&seq, 30.0, 40.0), [36.0]);
    }

    fn pt(tick: u32, value: f32, curve: f32) -> AutoPoint {
        AutoPoint { tick, value, curve }
    }

    #[test]
    fn automation_interpolates_with_curves_and_edges() {
        let lane = AutomationLane {
            target: AutoTarget::Tempo,
            scope: AutoScope::Song,
            points: vec![pt(96, 1.0, 0.0), pt(192, 3.0, 0.5), pt(288, 1.0, -0.5), pt(384, 5.0, 0.0)],
        };
        let v = |t: f64| lane.value_at(t).unwrap();
        // Avant le premier point et après le dernier : valeurs tenues.
        assert_eq!(v(0.0), 1.0);
        assert_eq!(v(-5.0), 1.0);
        assert_eq!(v(1_000.0), 5.0);
        assert_eq!(v(96.0), 1.0);
        assert_eq!(v(192.0), 3.0);
        assert_eq!(v(384.0), 5.0);
        // Linéaire.
        assert!((v(144.0) - 2.0).abs() < 1e-6);
        // Tension positive : départ lent, la descente 3 → 1 reste au-dessus de la droite.
        let slow = 0.5f64.powf(5f64.sqrt()) as f32;
        assert!((v(240.0) - (3.0 - 2.0 * slow)).abs() < 1e-5);
        assert!(v(240.0) > 2.0);
        // Tension négative : départ rapide, la montée 1 → 5 passe au-dessus de la droite.
        let fast = 0.5f64.powf(1.0 / 5f64.sqrt()) as f32;
        assert!((v(336.0) - (1.0 + 4.0 * fast)).abs() < 1e-5);
        assert!(v(336.0) > 3.0);
        // Monotone sur un segment courbé.
        let mut last = v(288.0);
        for t in 289..=384 {
            let x = v(t as f64);
            assert!(x >= last - 1e-6);
            last = x;
        }
        assert_eq!(shape_curve(0.0, 1.0), 0.0);
        assert_eq!(shape_curve(1.0, -1.0), 1.0);
        assert!((shape_curve(0.5, 1.0) - 0.5f64.powi(5)).abs() < 1e-12);

        let empty = AutomationLane { points: Vec::new(), ..lane.clone() };
        assert_eq!(empty.value_at(10.0), None);
        let single = AutomationLane { points: vec![pt(50, 0.3, 0.9)], ..lane };
        assert_eq!(single.value_at(0.0), Some(0.3));
        assert_eq!(single.value_at(99.0), Some(0.3));
    }

    #[test]
    fn automation_scopes() {
        let mut seq = Sequence {
            patterns: vec![pattern(&[], 96), pattern(&[], 192)],
            clips: vec![
                Clip { pattern: 1, start: 384, len: 384, offset: 48 },
                Clip { pattern: 0, start: 0, len: 96, offset: 0 },
            ],
            mode: PlayMode::Pattern(1),
            ..Default::default()
        };
        assert_eq!(seq.automation_tick(AutoScope::Song, 10.0), None);
        assert_eq!(seq.automation_tick(AutoScope::Pattern(0), 10.0), None);
        assert_eq!(seq.automation_tick(AutoScope::Pattern(1), 200.0), Some(8.0));
        seq.mode = PlayMode::Song;
        assert_eq!(seq.automation_tick(AutoScope::Song, 500.0), Some(500.0));
        // Relatif au clip, décalage compris, et en boucle dans le clip.
        assert_eq!(seq.automation_tick(AutoScope::Pattern(1), 384.0), Some(48.0));
        assert_eq!(seq.automation_tick(AutoScope::Pattern(1), 384.0 + 150.0), Some(6.0));
        assert_eq!(seq.automation_tick(AutoScope::Pattern(1), 300.0), None, "hors clip");
        assert_eq!(seq.automation_tick(AutoScope::Pattern(0), 50.0), Some(50.0));
        assert_eq!(seq.automation_tick(AutoScope::Pattern(7), 50.0), None);
    }
}
