//! Groove pool: grooves as objects of the project, shared by the patterns
//! (and the channels of a pattern) that play them.
//!
//! A groove is the timing offset, the velocity accent and the length change
//! of each sixteenth (or thirty-second) over one to four bars, plus how it is
//! applied: the grid notes are matched against (Base), how far they are first
//! pulled onto that grid (Quantize), how much of the groove's timing plays
//! (Timing), a humanizing jitter (Random) and how much of its accents play
//! (Velocity). The amounts belong to the groove, as in Ableton Live's pool:
//! every pattern using it changes when it changes.
//!
//! Grooves play without touching the notes (the sequencer hears the grooved
//! copy, built on the interface thread); committing writes the grooved notes,
//! with exactly the function playback uses.
//!
//! [`factory`] holds the built-in grooves, [`extract`] takes grooves from
//! notes, audio loops, clips and MIDI files.

pub mod extract;
pub mod factory;

use ponzik_engine::{Note, TICKS_PER_BAR, splitmix};

use crate::json::Json;
use crate::{PatternDef, Project};

pub use extract::{groove_from_audio, groove_from_loop, groove_from_midi, groove_warp_markers};

/// Identifier of a groove in the project's pool, saved in the file.
pub type GrooveId = u32;

/// Longest groove, in bars.
pub const MAX_GROOVE_BARS: u32 = 4;
/// Timing jitter at 100 % Random, in ticks either way (a thirty-second).
pub const RANDOM_TICKS: f32 = 12.0;
/// Velocity jitter at 100 % Random, as a ratio either way.
pub const RANDOM_VELOCITY: f32 = 0.15;
/// Sixteenths in a bar (the resolution of most grooves).
pub const GROOVE_STEPS: usize = 16;

/// Grid the notes are matched against: a note takes the groove's offset at
/// the grid point it belongs to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GrooveBase {
    Quarter,
    Eighth,
    EighthTriplet,
    #[default]
    Sixteenth,
    SixteenthTriplet,
    ThirtySecond,
}

impl GrooveBase {
    pub const ALL: [GrooveBase; 6] = [
        GrooveBase::Quarter,
        GrooveBase::Eighth,
        GrooveBase::EighthTriplet,
        GrooveBase::Sixteenth,
        GrooveBase::SixteenthTriplet,
        GrooveBase::ThirtySecond,
    ];

    /// Grid step, in ticks.
    pub fn ticks(self) -> u32 {
        match self {
            GrooveBase::Quarter => 96,
            GrooveBase::Eighth => 48,
            GrooveBase::EighthTriplet => 32,
            GrooveBase::Sixteenth => 24,
            GrooveBase::SixteenthTriplet => 16,
            GrooveBase::ThirtySecond => 12,
        }
    }

    /// Musical notation, the same in every language (also the saved id).
    pub fn label(self) -> &'static str {
        match self {
            GrooveBase::Quarter => "1/4",
            GrooveBase::Eighth => "1/8",
            GrooveBase::EighthTriplet => "1/8T",
            GrooveBase::Sixteenth => "1/16",
            GrooveBase::SixteenthTriplet => "1/16T",
            GrooveBase::ThirtySecond => "1/32",
        }
    }

    pub fn from_label(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|b| b.label() == s)
    }
}

/// A groove of the pool.
#[derive(Clone, Debug, PartialEq)]
pub struct Groove {
    /// Stable within the project (patterns refer to it).
    pub id: GrooveId,
    pub name: String,
    /// Steps per bar: 16 (sixteenths) or 32 (thirty-seconds).
    pub steps: u32,
    /// Timing offset of each step, in ticks (a sixteenth is 24), over
    /// `steps × bars` steps.
    pub timing: Vec<f32>,
    /// Velocity of each step relative to the average (1: unchanged).
    pub velocity: Vec<f32>,
    /// Length of the notes of each step, as a ratio (1: unchanged).
    pub duration: Vec<f32>,
    pub base: GrooveBase,
    /// How far notes are first pulled onto the base grid, 0 to 1.
    pub quantize: f32,
    /// How much of the timing (and length) offsets play, 0 to 1.
    pub amount: f32,
    /// Humanizing jitter of timing and velocity, 0 to 1.
    pub random: f32,
    /// How much of the velocity accents play, 0 to 1.
    pub velocity_amount: f32,
}

/// The grooves a pattern plays: one for the whole pattern, and channels of
/// the pattern playing their own (which wins over the pattern's).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PatternGroove {
    pub all: Option<GrooveId>,
    /// (channel, groove), at most one per channel, sorted by channel.
    pub channels: Vec<(usize, GrooveId)>,
}

impl PatternGroove {
    pub fn is_empty(&self) -> bool {
        self.all.is_none() && self.channels.is_empty()
    }

    /// Groove of channel `ch`: its own, else the pattern's.
    pub fn of(&self, ch: usize) -> Option<GrooveId> {
        self.channels.iter().find(|(c, _)| *c == ch).map(|&(_, g)| g).or(self.all)
    }
}

/// Grid point (a multiple of `g`) a tick belongs to: ticks up to half a step
/// after a point (included) and before it belong to it.
fn grid_point(tick: i64, g: i64) -> i64 {
    (tick + (g + 1) / 2 - 1).div_euclid(g) * g
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// A number in [-1, 1] from `seed`, always the same.
fn jitter(seed: u64) -> f32 {
    (splitmix(seed) >> 40) as f32 / (1u64 << 23) as f32 - 1.0
}

impl Groove {
    /// No offset at all, over `bars` bars of `steps` steps.
    pub fn straight(name: impl Into<String>, steps: u32, bars: u32) -> Self {
        let steps = if steps >= 32 { 32 } else { 16 };
        let n = (steps * bars.clamp(1, MAX_GROOVE_BARS)) as usize;
        Self {
            id: 0,
            name: name.into(),
            steps,
            timing: vec![0.0; n],
            velocity: vec![1.0; n],
            duration: vec![1.0; n],
            base: GrooveBase::default(),
            quantize: 0.0,
            amount: 1.0,
            random: 0.0,
            velocity_amount: 1.0,
        }
    }

    /// MPC swing: the second sixteenth of each eighth lands at `percent` of
    /// the eighth (50: straight, 66: triplet feel, 75: dotted).
    pub fn swing(percent: f32) -> Self {
        let p = percent.clamp(50.0, 75.0);
        let mut g = Self::straight(ponzik_i18n::tr_fmt!("MPC {}%", p.round() as u32), 16, 1);
        let late = (p / 50.0 - 1.0) * 24.0;
        for i in (1..16).step_by(2) {
            g.timing[i] = late;
        }
        g
    }

    pub fn bars(&self) -> u32 {
        (self.timing.len() as u32 / self.steps.max(1)).max(1)
    }

    /// Length of a step, in ticks.
    pub fn step_ticks(&self) -> u32 {
        TICKS_PER_BAR / self.steps.max(1)
    }

    /// Length of the whole groove, in ticks.
    pub fn len_ticks(&self) -> u32 {
        self.timing.len().max(1) as u32 * self.step_ticks()
    }

    /// Timing offset, velocity and length ratio at `tick` (any tick: the
    /// groove repeats), interpolated between its steps.
    pub fn point(&self, tick: i64) -> (f32, f32, f32) {
        let n = self.timing.len();
        if n == 0 {
            return (0.0, 1.0, 1.0);
        }
        let st = self.step_ticks() as i64;
        let phase = tick.rem_euclid(st * n as i64);
        let (k, frac) = ((phase / st) as usize, (phase % st) as f32 / st as f32);
        let k2 = (k + 1) % n;
        let at = |v: &[f32], d: f32| (v.get(k).copied().unwrap_or(d), v.get(k2).copied().unwrap_or(d));
        let (t0, t1) = at(&self.timing, 0.0);
        let (v0, v1) = at(&self.velocity, 1.0);
        let (d0, d1) = at(&self.duration, 1.0);
        (lerp(t0, t1, frac), lerp(v0, v1, frac), lerp(d0, d1, frac))
    }

    /// `note` as the groove plays it. `salt` varies the random jitter from
    /// one channel to another; the result never changes for the same input.
    pub fn apply(&self, note: &Note, salt: u64) -> Note {
        let g = self.base.ticks() as i64;
        let t = note.start as i64;
        let q = grid_point(t, g);
        let (off, vk, dk) = self.point(q);
        let quantize = self.quantize.clamp(0.0, 1.0);
        let amount = self.amount.clamp(0.0, 1.0);
        let random = self.random.clamp(0.0, 1.0);
        let mut pos = q as f32 + (t - q) as f32 * (1.0 - quantize) + off * amount;
        let mut vel = note.vel * (1.0 + (vk - 1.0) * self.velocity_amount.clamp(0.0, 1.0));
        if random > 0.0 {
            let seed = (u64::from(self.id) << 48) ^ (salt << 40) ^ (u64::from(note.key) << 32) ^ u64::from(note.start);
            pos += jitter(seed) * RANDOM_TICKS * random;
            vel *= 1.0 + jitter(seed ^ 0x5bd1_e995) * RANDOM_VELOCITY * random;
        }
        let len = (note.len as f32 * (1.0 + (dk - 1.0) * amount)).round().max(1.0) as u32;
        Note { start: pos.round().max(0.0) as u32, len, vel: vel.clamp(0.01, 1.0), ..*note }
    }

    /// Every note as the groove plays it, sorted by start.
    pub fn apply_notes(&self, notes: &[Note], salt: u64) -> Vec<Note> {
        let mut v: Vec<Note> = notes.iter().map(|n| self.apply(n, salt)).collect();
        v.sort_by_key(|n| (n.start, n.key));
        v
    }

    /// Largest timing offset, in ticks.
    pub fn depth(&self) -> f32 {
        self.timing.iter().fold(0.0f32, |m, t| m.max(t.abs()))
    }

    /// Same offsets, accents and lengths (name, id and amounts aside).
    pub fn same_feel(&self, o: &Groove) -> bool {
        self.steps == o.steps && self.timing == o.timing && self.velocity == o.velocity && self.duration == o.duration
    }

    /// Resizes the step lists to `steps × bars` (bars from 1 to 4), repeating
    /// the existing bars: a malformed or edited groove stays consistent.
    pub fn normalize(&mut self) {
        self.steps = if self.steps >= 32 { 32 } else { 16 };
        let s = self.steps as usize;
        let bars = self.timing.len().div_ceil(s).clamp(1, MAX_GROOVE_BARS as usize);
        let n = s * bars;
        for (v, d) in [(&mut self.timing, 0.0), (&mut self.velocity, 1.0), (&mut self.duration, 1.0)] {
            if v.is_empty() {
                v.push(d);
            }
            let have = v.len();
            if have < n {
                for i in have..n {
                    let x = v[i % have];
                    v.push(x);
                }
            }
            v.truncate(n);
        }
        let tick = TICKS_PER_BAR as f32 / 2.0;
        for t in &mut self.timing {
            *t = if t.is_finite() { t.clamp(-tick, tick) } else { 0.0 };
        }
        for v in self.velocity.iter_mut().chain(self.duration.iter_mut()) {
            *v = if v.is_finite() { v.clamp(0.0, 4.0) } else { 1.0 };
        }
        for a in [&mut self.quantize, &mut self.amount, &mut self.random, &mut self.velocity_amount] {
            *a = if a.is_finite() { a.clamp(0.0, 1.0) } else { 0.0 };
        }
    }
}

impl PatternDef {
    /// Groove id channel `ch` plays in this pattern.
    pub fn groove_id(&self, ch: usize) -> Option<GrooveId> {
        self.groove.as_ref().and_then(|g| g.of(ch))
    }
}

/// Who plays a groove: a whole pattern, or one channel of a pattern.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GrooveUser {
    pub pattern: usize,
    pub channel: Option<usize>,
}

impl Project {
    pub fn groove(&self, id: GrooveId) -> Option<&Groove> {
        self.grooves.iter().find(|g| g.id == id)
    }

    pub fn groove_mut(&mut self, id: GrooveId) -> Option<&mut Groove> {
        self.grooves.iter_mut().find(|g| g.id == id)
    }

    /// Puts `g` in the pool under a new id; returns it.
    pub fn add_groove(&mut self, mut g: Groove) -> GrooveId {
        g.id = self.grooves.iter().map(|g| g.id + 1).max().unwrap_or(1).max(1);
        g.normalize();
        let id = g.id;
        self.grooves.push(g);
        id
    }

    /// The pool's groove with the same feel as `g`, or `g` added to it.
    pub fn find_or_add_groove(&mut self, g: Groove) -> GrooveId {
        match self.grooves.iter().find(|p| p.same_feel(&g) && p.name == g.name) {
            Some(p) => p.id,
            None => self.add_groove(g),
        }
    }

    /// Removes a groove from the pool; its patterns play straight again.
    pub fn remove_groove(&mut self, id: GrooveId) -> bool {
        let before = self.grooves.len();
        self.grooves.retain(|g| g.id != id);
        for p in &mut self.patterns {
            if let Some(pg) = p.groove.as_mut() {
                if pg.all == Some(id) {
                    pg.all = None;
                }
                pg.channels.retain(|&(_, g)| g != id);
            }
            if p.groove.as_ref().is_some_and(PatternGroove::is_empty) {
                p.groove = None;
            }
        }
        self.grooves.len() != before
    }

    /// Groove that channel `channel` plays in pattern `pattern`.
    pub fn groove_of(&self, pattern: usize, channel: usize) -> Option<&Groove> {
        self.patterns.get(pattern).and_then(|p| p.groove_id(channel)).and_then(|id| self.groove(id))
    }

    /// Sets the groove of a whole pattern (`None`: straight); the channels
    /// playing their own keep it.
    pub fn set_pattern_groove(&mut self, pattern: usize, id: Option<GrooveId>) {
        let Some(p) = self.patterns.get_mut(pattern) else { return };
        let mut pg = p.groove.take().unwrap_or_default();
        pg.all = id;
        p.groove = (!pg.is_empty()).then_some(pg);
    }

    /// Sets the groove of one channel in a pattern (`None`: the pattern's).
    pub fn set_channel_groove(&mut self, pattern: usize, channel: usize, id: Option<GrooveId>) {
        let Some(p) = self.patterns.get_mut(pattern) else { return };
        let mut pg = p.groove.take().unwrap_or_default();
        pg.channels.retain(|&(c, _)| c != channel);
        if let Some(id) = id {
            pg.channels.push((channel, id));
            pg.channels.sort_by_key(|&(c, _)| c);
        }
        p.groove = (!pg.is_empty()).then_some(pg);
    }

    /// Patterns and channels playing groove `id`.
    pub fn groove_users(&self, id: GrooveId) -> Vec<GrooveUser> {
        let mut v = Vec::new();
        for (i, p) in self.patterns.iter().enumerate() {
            let Some(pg) = &p.groove else { continue };
            if pg.all == Some(id) {
                v.push(GrooveUser { pattern: i, channel: None });
            }
            v.extend(pg.channels.iter().filter(|&&(_, g)| g == id).map(|&(c, _)| GrooveUser { pattern: i, channel: Some(c) }));
        }
        v
    }

    /// Notes of channel `channel` of `p` as they play (its groove applied).
    pub fn grooved_notes(&self, p: &PatternDef, channel: usize) -> Vec<Note> {
        let notes = p.notes.get(channel).map_or(&[][..], |n| n.as_slice());
        match p.groove_id(channel).and_then(|id| self.groove(id)) {
            Some(g) => g.apply_notes(notes, channel as u64),
            None => notes.to_vec(),
        }
    }

    /// Notes of pattern `pattern` on channel `channel` as they play.
    pub fn played_notes(&self, pattern: usize, channel: usize) -> Vec<Note> {
        self.patterns.get(pattern).map_or_else(Vec::new, |p| self.grooved_notes(p, channel))
    }

    /// Writes the grooves of pattern `pattern` into its notes (each channel
    /// exactly as it plays) and removes them. False without a groove.
    pub fn commit_groove(&mut self, pattern: usize) -> bool {
        let Some(p) = self.patterns.get(pattern) else { return false };
        if p.groove.is_none() {
            return false;
        }
        let played: Vec<Vec<Note>> = (0..p.notes.len()).map(|ch| self.grooved_notes(p, ch)).collect();
        let p = &mut self.patterns[pattern];
        p.notes = played;
        p.groove = None;
        true
    }

    /// Saves the groove of the notes of pattern `pattern` (all channels) in
    /// the pool; its id, `None` for an empty pattern.
    pub fn extract_groove(&mut self, pattern: usize) -> Option<GrooveId> {
        let p = self.patterns.get(pattern)?;
        let bars = p.len.div_ceil(TICKS_PER_BAR);
        let g = extract::from_notes(p.name.clone(), p.notes.iter().flatten(), bars)?;
        Some(self.add_groove(g))
    }

    /// Channel `idx` is gone: the grooves of the channels after it follow them.
    pub(crate) fn grooves_channel_removed(&mut self, idx: usize) {
        for p in &mut self.patterns {
            let Some(pg) = p.groove.as_mut() else { continue };
            pg.channels.retain(|&(c, _)| c != idx);
            for (c, _) in &mut pg.channels {
                if *c > idx {
                    *c -= 1;
                }
            }
            if pg.is_empty() {
                p.groove = None;
            }
        }
    }
}

// --- File format ---

fn obj(pairs: Vec<(&str, Json)>) -> Json {
    Json::Obj(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

fn nums(v: &[f32]) -> Json {
    Json::Arr(v.iter().map(|&x| ((x * 1000.0).round() / 1000.0).into()).collect())
}

fn groove_json(g: &Groove) -> Json {
    obj(vec![
        ("id", g.id.into()),
        ("name", g.name.as_str().into()),
        ("steps", g.steps.into()),
        ("timing", nums(&g.timing)),
        ("velocity", nums(&g.velocity)),
        ("duration", nums(&g.duration)),
        ("base", g.base.label().into()),
        ("quantize", g.quantize.into()),
        ("amount", g.amount.into()),
        ("random", g.random.into()),
        ("velocity_amount", g.velocity_amount.into()),
    ])
}

/// The pool, as saved.
pub(crate) fn pool_json(pool: &[Groove]) -> Json {
    Json::Arr(pool.iter().map(groove_json).collect())
}

/// `"groove": id` and `"channel_grooves": [[channel, id], …]` of a pattern.
pub(crate) fn pattern_groove_json(p: &PatternDef, pairs: &mut Vec<(&'static str, Json)>) {
    let Some(pg) = &p.groove else { return };
    if let Some(id) = pg.all {
        pairs.push(("groove", id.into()));
    }
    if !pg.channels.is_empty() {
        let v = pg.channels.iter().map(|&(c, g)| Json::Arr(vec![c.into(), g.into()])).collect();
        pairs.push(("channel_grooves", Json::Arr(v)));
    }
}

fn f32_at(v: Option<&Json>, default: f32) -> f32 {
    v.and_then(Json::as_f64).filter(|n| n.is_finite()).map_or(default, |n| n.clamp(-1e6, 1e6) as f32)
}

fn id_at(v: Option<&Json>) -> Option<u32> {
    v.and_then(Json::as_f64).filter(|n| *n >= 0.0 && n.fract() == 0.0 && *n <= u32::MAX as f64).map(|n| n as u32)
}

/// A saved groove; older files wrote 16 timings and velocities without an
/// id or amounts (`amounts` then gives them: the pattern's).
fn load_groove(v: &Json, amounts: (f32, f32)) -> Option<Groove> {
    let list = |k: &str, d: f32| -> Vec<f32> {
        v.get(k).and_then(Json::as_arr).unwrap_or(&[]).iter().map(|x| f32_at(Some(x), d)).collect()
    };
    let (timing, velocity, duration) = (list("timing", 0.0), list("velocity", 1.0), list("duration", 1.0));
    if timing.is_empty() && velocity.is_empty() {
        return None;
    }
    let steps = f32_at(v.get("steps"), 16.0) as u32;
    let len = timing.len().max(velocity.len());
    let mut g = Groove::straight(v.get("name").and_then(Json::as_str).unwrap_or("Groove"), steps, 1);
    let fill = |mut x: Vec<f32>, d: f32| {
        x.resize(len, d);
        x
    };
    g.timing = fill(timing, 0.0);
    g.velocity = fill(velocity, 1.0);
    g.duration = fill(duration, 1.0);
    g.id = id_at(v.get("id")).unwrap_or(0);
    g.base = v.get("base").and_then(Json::as_str).and_then(GrooveBase::from_label).unwrap_or_default();
    g.quantize = f32_at(v.get("quantize"), 0.0);
    g.amount = f32_at(v.get("amount").or(v.get("timing_amount")), amounts.0);
    g.random = f32_at(v.get("random"), 0.0);
    g.velocity_amount = f32_at(v.get("velocity_amount"), amounts.1);
    g.normalize();
    Some(g)
}

/// Loads the pool and the grooves of the patterns (`root.patterns[i]`, in
/// the order of `p.patterns`). Grooves of older files, copied into each
/// pattern, join the pool (once for identical ones); ids that are taken
/// twice or point nowhere are dropped.
pub(crate) fn load_grooves(p: &mut Project, root: &Json) {
    p.grooves.clear();
    for v in root.get("grooves").and_then(Json::as_arr).unwrap_or(&[]) {
        let Some(mut g) = load_groove(v, (1.0, 1.0)) else { continue };
        if g.id == 0 || p.groove(g.id).is_some() {
            p.add_groove(g);
        } else {
            g.normalize();
            p.grooves.push(g);
        }
    }
    let saved = root.get("patterns").and_then(Json::as_arr).unwrap_or(&[]);
    let channels = p.channels.len();
    for (i, v) in saved.iter().enumerate().take(p.patterns.len()) {
        let mut pg = PatternGroove::default();
        match v.get("groove") {
            Some(Json::Obj(_)) => {
                let g = v.get("groove").unwrap_or(&Json::Null);
                let amounts = (f32_at(g.get("timing_amount"), 1.0), f32_at(g.get("velocity_amount"), 1.0));
                if let Some(mut legacy) = load_groove(g, amounts) {
                    legacy.id = 0;
                    let found = p.grooves.iter().find(|o| {
                        o.same_feel(&legacy) && o.name == legacy.name && o.amount == legacy.amount && o.velocity_amount == legacy.velocity_amount
                    });
                    pg.all = Some(match found {
                        Some(o) => o.id,
                        None => p.add_groove(legacy),
                    });
                }
            }
            other => pg.all = id_at(other).filter(|&id| p.groove(id).is_some()),
        }
        for e in v.get("channel_grooves").and_then(Json::as_arr).unwrap_or(&[]) {
            let e = e.as_arr().unwrap_or(&[]);
            let (Some(c), Some(id)) = (id_at(e.first()), id_at(e.get(1))) else { continue };
            let c = c as usize;
            if c < channels && p.groove(id).is_some() && pg.channels.iter().all(|&(o, _)| o != c) {
                pg.channels.push((c, id));
            }
        }
        pg.channels.sort_by_key(|&(c, _)| c);
        p.patterns[i].groove = (!pg.is_empty()).then_some(pg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InstrumentDef;

    fn project_with(starts: &[u32]) -> Project {
        let mut p = Project::empty();
        p.add_channel("A", InstrumentDef::new("pulse"), 60);
        for &s in starts {
            p.add_note(0, 0, Note::new(s, 12, 60, 0.5));
        }
        p
    }

    fn starts(notes: &[Note]) -> Vec<u32> {
        notes.iter().map(|n| n.start).collect()
    }

    #[test]
    fn mpc_swing_delays_the_off_sixteenths() {
        let g = Groove::swing(66.0);
        assert!((g.timing[1] - 0.32 * 24.0).abs() < 1e-4);
        assert_eq!(g.timing[0], 0.0);
        assert_eq!(Groove::swing(50.0).depth(), 0.0);
        assert_eq!(Groove::swing(75.0).timing[3], 12.0);
    }

    #[test]
    fn grid_points_take_late_hits_up_to_half_a_step() {
        assert_eq!(grid_point(0, 24), 0);
        assert_eq!(grid_point(36, 24), 24);
        assert_eq!(grid_point(37, 24), 48);
        assert_eq!(grid_point(20, 24), 24);
        assert_eq!(grid_point(-1, 24), 0);
        assert_eq!(grid_point(48, 32), 32);
        assert_eq!(grid_point(49, 32), 64);
    }

    #[test]
    fn zero_amounts_are_the_identity() {
        let notes: Vec<Note> = (0..32).map(|i| Note::new(i * 13 + (i % 3), 10 + i, 36 + (i % 12) as u8, 0.3 + 0.02 * i as f32)).collect();
        for f in factory::library() {
            let g = Groove { amount: 0.0, velocity_amount: 0.0, quantize: 0.0, random: 0.0, ..f };
            for n in &notes {
                assert_eq!(g.apply(n, 3), *n, "{}", g.name);
            }
        }
    }

    #[test]
    fn full_timing_moves_notes_onto_the_groove() {
        let mut g = Groove::straight("x", 16, 1);
        for (i, t) in g.timing.iter_mut().enumerate() {
            *t = (i as f32 * 1.7).sin() * 9.0;
        }
        g.quantize = 1.0;
        // Notes a little off every sixteenth: quantized, then on the groove's points.
        for s in 0..32u32 {
            let off = [3i32, -5, 0, 7][s as usize % 4];
            let n = Note::new((s as i32 * 24 + off).max(0) as u32, 12, 60, 0.5);
            let want = s as f32 * 24.0 + g.timing[s as usize % 16];
            assert_eq!(g.apply(&n, 0).start, want.round().max(0.0) as u32, "{s}");
        }
        // Without quantize, a note keeps its distance to the grid.
        g.quantize = 0.0;
        let n = Note::new(24 * 5 + 3, 12, 60, 0.5);
        assert_eq!(g.apply(&n, 0).start, (24.0 * 5.0 + 3.0 + g.timing[5]).round() as u32);
        // Half the timing: half the offset.
        let swing = Groove { amount: 0.5, ..Groove::swing(75.0) };
        assert_eq!(swing.apply(&Note::new(24, 12, 60, 0.5), 0).start, 30);
        // Notes before the grid stay on it: no negative start.
        let early = Groove { timing: vec![-30.0; 16], ..Groove::straight("e", 16, 1) };
        assert_eq!(early.apply(&Note::new(0, 12, 60, 0.5), 0).start, 0);
    }

    #[test]
    fn base_picks_the_grid_points_that_move() {
        // A sixteenth swing on an eighth base: only the eighths are matched,
        // and they carry no offset.
        let g = Groove { base: GrooveBase::Eighth, ..Groove::swing(66.0) };
        assert_eq!(g.apply(&Note::new(24, 12, 60, 0.5), 0).start, 24);
        // On a thirty-second base, the thirty-second between two steps takes
        // half of the swing.
        let g = Groove { base: GrooveBase::ThirtySecond, ..Groove::swing(75.0) };
        assert_eq!(g.apply(&Note::new(12, 6, 60, 0.5), 0).start, 18);
        assert_eq!(g.apply(&Note::new(24, 6, 60, 0.5), 0).start, 36);
    }

    #[test]
    fn velocity_and_duration_follow_their_amounts() {
        let mut g = Groove::straight("v", 16, 1);
        g.velocity[0] = 1.5;
        g.duration[0] = 0.5;
        let n = Note::new(0, 20, 60, 0.4);
        assert!((g.apply(&n, 0).vel - 0.6).abs() < 1e-5);
        assert_eq!(g.apply(&n, 0).len, 10);
        g.velocity_amount = 0.5;
        g.amount = 0.5;
        assert!((g.apply(&n, 0).vel - 0.5).abs() < 1e-5);
        assert_eq!(g.apply(&n, 0).len, 15);
    }

    #[test]
    fn random_is_bounded_and_repeatable() {
        let g = Groove { random: 1.0, ..Groove::straight("r", 16, 1) };
        let mut moved = 0;
        for s in 0..64u32 {
            let n = Note::new(96 + s * 24, 12, 60, 0.5);
            let a = g.apply(&n, 1);
            assert_eq!(a, g.apply(&n, 1));
            let d = a.start as f32 - n.start as f32;
            assert!(d.abs() <= RANDOM_TICKS + 0.5, "{d}");
            assert!((a.vel / n.vel - 1.0).abs() <= RANDOM_VELOCITY + 1e-4);
            moved += (d != 0.0) as u32;
        }
        assert!(moved > 40, "{moved}");
    }

    #[test]
    fn commit_equals_playback() {
        let mut p = project_with(&[0, 23, 49, 75, 96, 130, 144, 170, 200, 260, 300, 350]);
        p.add_channel("B", InstrumentDef::new("pulse"), 60);
        for s in 0..8 {
            p.add_note(0, 1, Note::new(s * 48 + 24, 12, 42, 0.7));
        }
        let mut g = factory::library().into_iter().find(|g| g.steps == 16 && g.bars() == 2).unwrap();
        g.random = 0.4;
        g.quantize = 0.3;
        let a = p.add_groove(g);
        let b = p.add_groove(Groove::swing(62.0));
        p.set_pattern_groove(0, Some(a));
        p.set_channel_groove(0, 1, Some(b));
        let played: Vec<Vec<Note>> = (0..2).map(|ch| p.played_notes(0, ch)).collect();
        let seq = p.sequence();
        for (ch, want) in played.iter().enumerate() {
            let mut s = seq.patterns[0].notes[ch].clone();
            s.sort_by_key(|n| (n.start, n.key));
            assert_eq!(&s, want);
        }
        assert!(p.commit_groove(0));
        assert!(p.patterns[0].groove.is_none());
        for (ch, want) in played.iter().enumerate() {
            assert_eq!(&p.patterns[0].notes[ch], want);
            assert_eq!(&p.played_notes(0, ch), want);
        }
        assert!(!p.commit_groove(0));
    }

    #[test]
    fn shared_grooves_change_every_user() {
        let mut p = project_with(&[0, 24, 48, 72]);
        p.patterns.push(p.patterns[0].clone());
        p.patterns.push(p.patterns[0].clone());
        let id = p.add_groove(Groove::swing(58.0));
        p.set_pattern_groove(0, Some(id));
        p.set_pattern_groove(1, Some(id));
        p.set_channel_groove(2, 0, Some(id));
        assert_eq!(p.groove_users(id).len(), 3);
        assert_eq!(starts(&p.played_notes(1, 0)), [0, 28, 48, 76]);
        p.groove_mut(id).unwrap().timing[1] = 12.0;
        p.groove_mut(id).unwrap().timing[3] = 12.0;
        for pat in 0..3 {
            assert_eq!(starts(&p.played_notes(pat, 0)), [0, 36, 48, 84], "{pat}");
        }
        p.groove_mut(id).unwrap().amount = 0.0;
        for pat in 0..3 {
            assert_eq!(starts(&p.played_notes(pat, 0)), [0, 24, 48, 72]);
        }
        assert!(p.remove_groove(id));
        assert!(p.patterns.iter().all(|pt| pt.groove.is_none()));
    }

    #[test]
    fn channel_grooves_follow_removed_channels() {
        let mut p = project_with(&[0]);
        p.add_channel("B", InstrumentDef::new("pulse"), 60);
        p.add_channel("C", InstrumentDef::new("pulse"), 60);
        let id = p.add_groove(Groove::swing(66.0));
        p.set_channel_groove(0, 0, Some(id));
        p.set_channel_groove(0, 2, Some(id));
        p.remove_channel(0);
        assert_eq!(p.patterns[0].groove.as_ref().unwrap().channels, [(1, id)]);
        p.remove_channel(1);
        assert!(p.patterns[0].groove.is_none());
    }

    #[test]
    fn extract_and_reuse() {
        let mut p = project_with(&[0, 36, 48, 84]);
        let id = p.extract_groove(0).unwrap();
        assert_eq!(p.groove(id).unwrap().timing[1], 12.0);
        let f = factory::library().remove(0);
        let a = p.find_or_add_groove(f.clone());
        assert_eq!(p.find_or_add_groove(f), a);
        assert_ne!(a, id);
        assert_eq!(p.grooves.len(), 2);
    }
}
