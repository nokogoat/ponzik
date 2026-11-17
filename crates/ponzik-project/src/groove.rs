//! Groove pool: timing and velocity offsets of each sixteenth of a bar,
//! taken from a pattern or from the attacks of a recording, and applied to
//! patterns with an amount.
//!
//! A pattern's groove plays without touching its notes (the sequencer hears
//! the grooved copy); committing it writes the grooved notes.

use ponzik_engine::{Note, TICKS_PER_BAR, TICKS_PER_STEP};
use ponzik_i18n::tr_fmt;

use crate::{AudioClipDef, Project, TempoMap};

/// Sixteenths in a groove: one bar.
pub const GROOVE_STEPS: usize = 16;
/// Swing amounts of the built-in grooves, in percent (MPC style).
pub const SWINGS: [u8; 7] = [50, 54, 58, 62, 66, 71, 75];

/// Offsets of each sixteenth of a bar.
#[derive(Clone, Debug, PartialEq)]
pub struct Groove {
    pub name: String,
    /// Timing offset of each sixteenth, in ticks (a sixteenth is
    /// [`TICKS_PER_STEP`]).
    pub timing: [f32; GROOVE_STEPS],
    /// Velocity of each sixteenth relative to the average (1: unchanged).
    pub velocity: [f32; GROOVE_STEPS],
}

/// A pattern's groove and how much of it plays, both in [0, 1].
#[derive(Clone, Debug, PartialEq)]
pub struct PatternGroove {
    pub groove: Groove,
    pub timing: f32,
    pub velocity: f32,
}

/// Sixteenth of a bar a tick belongs to, and its distance to it: ticks up to
/// half a step after a sixteenth (included) and before it belong to it.
fn slot(tick: u32) -> (usize, i64) {
    let step = TICKS_PER_STEP as i64;
    let t = tick as i64;
    let g = (t + step / 2 - 1).div_euclid(step);
    ((g.rem_euclid(GROOVE_STEPS as i64)) as usize, t - g * step)
}

impl Groove {
    /// No offset at all.
    pub fn straight(name: impl Into<String>) -> Self {
        Self { name: name.into(), timing: [0.0; GROOVE_STEPS], velocity: [1.0; GROOVE_STEPS] }
    }

    /// MPC swing: the second sixteenth of each eighth lands at `percent` of
    /// the eighth (50: straight, 66: triplet feel, 75: dotted).
    pub fn swing(percent: f32) -> Self {
        let p = percent.clamp(50.0, 75.0);
        let mut g = Self::straight(tr_fmt!("Swing {}%", p.round() as u32));
        let late = (p / 50.0 - 1.0) * TICKS_PER_STEP as f32;
        for i in (1..GROOVE_STEPS).step_by(2) {
            g.timing[i] = late;
        }
        g
    }

    /// The built-in grooves: MPC swings from 50 to 75 %.
    pub fn builtins() -> Vec<Groove> {
        SWINGS.iter().map(|&p| Self::swing(p as f32)).collect()
    }

    /// Groove of hits at (tick, velocity), bar after bar: the average offset
    /// and the relative velocity of the hits of each sixteenth. `None`
    /// without any hit.
    pub fn from_hits(name: impl Into<String>, hits: impl IntoIterator<Item = (u32, f32)>) -> Option<Self> {
        let mut dev = [0.0f64; GROOVE_STEPS];
        let mut vel = [0.0f64; GROOVE_STEPS];
        let mut count = [0u32; GROOVE_STEPS];
        let (mut total, mut hits_n) = (0.0f64, 0u32);
        for (tick, v) in hits {
            let (s, d) = slot(tick);
            dev[s] += d as f64;
            vel[s] += v.max(0.0) as f64;
            count[s] += 1;
            total += v.max(0.0) as f64;
            hits_n += 1;
        }
        if hits_n == 0 {
            return None;
        }
        let mean = total / hits_n as f64;
        let mut g = Self::straight(name);
        for s in 0..GROOVE_STEPS {
            if count[s] > 0 {
                g.timing[s] = (dev[s] / count[s] as f64) as f32;
                if mean > 0.0 {
                    g.velocity[s] = (vel[s] / count[s] as f64 / mean) as f32;
                }
            }
        }
        Some(g)
    }

    /// Groove of notes (every channel together).
    pub fn from_notes<'a>(name: impl Into<String>, notes: impl IntoIterator<Item = &'a Note>) -> Option<Self> {
        Self::from_hits(name, notes.into_iter().filter(|n| !n.slide).map(|n| (n.start, n.vel)))
    }

    /// `note` played with `timing` and `velocity` of the groove (0: none, 1: all).
    pub fn apply(&self, note: &Note, timing: f32, velocity: f32) -> Note {
        let (s, _) = slot(note.start);
        let shift = (self.timing[s] * timing.clamp(0.0, 1.0)).round() as i64;
        let k = 1.0 + (self.velocity[s] - 1.0) * velocity.clamp(0.0, 1.0);
        Note { start: (note.start as i64 + shift).max(0) as u32, vel: (note.vel * k).clamp(0.01, 1.0), ..*note }
    }

    /// Largest timing offset, in ticks.
    pub fn depth(&self) -> f32 {
        self.timing.iter().fold(0.0f32, |m, t| m.max(t.abs()))
    }
}

impl PatternGroove {
    pub fn apply(&self, notes: &[Note]) -> Vec<Note> {
        notes.iter().map(|n| self.groove.apply(n, self.timing, self.velocity)).collect()
    }
}

/// Groove of the attacks of an audio clip playing `sample`: each attack
/// placed on the song's timeline (the clip's warp or the tempo map), its
/// peak level as velocity. `None` without attacks.
pub fn groove_from_audio(clip: &AudioClipDef, sample: &ponzik_engine::Sample, map: &TempoMap, name: impl Into<String>) -> Option<Groove> {
    let sr = sample.sample_rate.max(1.0) as f64;
    let mono = ponzik_dsp::onset::mono(&sample.left, &sample.right);
    let analysis = ponzik_dsp::onset::OnsetAnalysis::new(&mono, sample.sample_rate);
    let onsets = analysis.pick(&mono, 0.6, 4_096);
    let warp = clip.warp_map();
    let clip_secs = map.secs_at(clip.start as f64);
    let peak_span = (0.01 * sr) as usize;
    let hits = onsets.iter().filter_map(|o| {
        let secs = o.pos as f64 / sr;
        let tick = if clip.warp {
            clip.start as f64 + (warp.beat_at(secs) - warp.beat_at(clip.offset)) * ponzik_engine::PPQ as f64
        } else {
            map.tick_at_secs(clip_secs + secs - clip.offset)
        };
        let local = tick - clip.start as f64;
        if local < -0.5 || local >= clip.len as f64 {
            return None;
        }
        let peak = mono[o.pos..(o.pos + peak_span).min(mono.len())].iter().fold(0.0f32, |m, x| m.max(x.abs()));
        Some((local.round().max(0.0) as u32, peak))
    });
    Groove::from_hits(name, hits.collect::<Vec<_>>())
}

/// Bar length used by the grooves.
pub const GROOVE_BAR: u32 = TICKS_PER_BAR;

impl Project {
    /// The grooves offered: the built-in swings, then the project's own.
    pub fn groove_pool(&self) -> Vec<Groove> {
        Groove::builtins().into_iter().chain(self.grooves.iter().cloned()).collect()
    }

    /// Notes of pattern `pattern` on channel `channel` as they play (groove applied).
    pub fn played_notes(&self, pattern: usize, channel: usize) -> Vec<Note> {
        let Some(p) = self.patterns.get(pattern) else { return Vec::new() };
        let notes = p.notes.get(channel).map_or(&[][..], |n| n.as_slice());
        match &p.groove {
            Some(g) => g.apply(notes),
            None => notes.to_vec(),
        }
    }

    /// Writes the groove of pattern `pattern` into its notes and removes it.
    pub fn commit_groove(&mut self, pattern: usize) -> bool {
        let Some(p) = self.patterns.get_mut(pattern) else { return false };
        let Some(g) = p.groove.take() else { return false };
        for notes in &mut p.notes {
            *notes = g.apply(notes);
            notes.sort_by_key(|n| (n.start, n.key));
        }
        true
    }

    /// Saves the groove of the notes of pattern `pattern` in the project's
    /// pool; its index there, `None` for an empty pattern.
    pub fn extract_groove(&mut self, pattern: usize) -> Option<usize> {
        let p = self.patterns.get(pattern)?;
        let g = Groove::from_notes(p.name.clone(), p.notes.iter().flatten())?;
        self.grooves.push(g);
        Some(self.grooves.len() - 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mpc_swing_delays_the_off_sixteenths() {
        let g = Groove::swing(66.0);
        assert!((g.timing[1] - 0.32 * 24.0).abs() < 1e-4);
        assert_eq!(g.timing[0], 0.0);
        assert_eq!(Groove::swing(50.0).depth(), 0.0);
        assert_eq!(Groove::swing(75.0).timing[3], 12.0);
        assert_eq!(Groove::builtins().len(), SWINGS.len());
    }

    #[test]
    fn slots_take_late_hits_up_to_half_a_step() {
        assert_eq!(slot(0), (0, 0));
        assert_eq!(slot(36), (1, 12));
        assert_eq!(slot(37), (2, -11));
        assert_eq!(slot(20), (1, -4));
        assert_eq!(slot(384 + 24), (1, 0));
        assert_eq!(slot(383), (0, -1));
    }

    #[test]
    fn extracted_groove_matches_the_played_offsets() {
        // A hi-hat line swung at 62 % with accents on the beats, over two bars.
        let swing: f32 = (0.62 / 0.5 - 1.0) * 24.0;
        let mut notes = Vec::new();
        for s in 0..32u32 {
            let late = if s % 2 == 1 { swing.round() as u32 } else { 0 };
            let vel = if s % 4 == 0 { 1.0 } else { 0.5 };
            notes.push(Note::new(s * 24 + late, 12, 42, vel));
        }
        let g = Groove::from_notes("hats", &notes).unwrap();
        for s in 0..16 {
            assert_eq!(g.timing[s], if s % 2 == 1 { swing.round() } else { 0.0 }, "{s}");
        }
        // Mean velocity: (4 × 1 + 12 × 0.5) / 16 = 0.625.
        assert!((g.velocity[0] - 1.6).abs() < 1e-5);
        assert!((g.velocity[1] - 0.8).abs() < 1e-5);
        assert!(Groove::from_notes("none", &[]).is_none());

        // Applied to straight notes: the same offsets, scaled by the amount.
        let straight = Note::new(24 * 5, 12, 60, 0.5);
        assert_eq!(g.apply(&straight, 1.0, 0.0).start, 24 * 5 + 6);
        assert_eq!(g.apply(&straight, 0.5, 0.0).start, 24 * 5 + 3);
        assert_eq!(g.apply(&straight, 0.0, 0.0), straight);
        let beat = Note::new(96, 12, 60, 0.5);
        assert!((g.apply(&beat, 0.0, 1.0).vel - 0.8).abs() < 1e-5);
        assert!((g.apply(&beat, 0.0, 0.5).vel - 0.65).abs() < 1e-5);
        // Notes before the grid stay on it: no negative start.
        let early = Groove { timing: [-30.0; GROOVE_STEPS], ..Groove::straight("x") };
        assert_eq!(early.apply(&Note::new(0, 12, 60, 0.5), 1.0, 0.0).start, 0);
    }

    #[test]
    fn groove_of_a_recorded_beat() {
        // 120 BPM (250 samples a tick at 48 kHz): clicks on every sixteenth,
        // the off ones 8 ticks late, the beats louder; the clip starts on bar 2.
        let sr = 48_000.0;
        let mut left = vec![0.0f32; 48_000 * 2];
        for s in 0..16 {
            let tick = s * 24 + if s % 2 == 1 { 8 } else { 0 };
            let level = if s % 4 == 0 { 0.9 } else { 0.45 };
            let at = tick as usize * 250;
            for i in 0..400 {
                left[at + i] = level * (-(i as f32) / 80.0).exp() * if i % 2 == 0 { 1.0 } else { -1.0 };
            }
        }
        let sample = ponzik_engine::Sample { name: "beat".into(), sample_rate: sr, right: left.clone(), left, markers: Vec::new() };
        let clip = AudioClipDef { start: 384, len: 384 * 2, ..Default::default() };
        let map = TempoMap::new(vec![ponzik_engine::TempoPoint { tick: 0, bpm: 120.0, ramp: false }], Vec::new());
        let g = groove_from_audio(&clip, &sample, &map, "beat").unwrap();
        for s in 0..16 {
            let want = if s % 2 == 1 { 8.0 } else { 0.0 };
            assert!((g.timing[s] - want).abs() <= 1.0, "{s}: {:?}", g.timing);
        }
        assert!(g.velocity[0] > 1.3 && g.velocity[1] < 0.9, "{:?}", g.velocity);
    }

    #[test]
    fn pattern_grooves_play_and_commit() {
        let mut p = crate::Project::empty();
        p.add_channel("A", crate::InstrumentDef::new("pulse"), 60);
        for s in 0..4 {
            p.add_note(0, 0, Note::new(s * 24, 12, 60, 0.8));
        }
        p.patterns[0].groove = Some(PatternGroove { groove: Groove::swing(75.0), timing: 1.0, velocity: 1.0 });
        let played: Vec<u32> = p.played_notes(0, 0).iter().map(|n| n.start).collect();
        assert_eq!(played, [0, 36, 48, 84]);
        // The sequence plays the grooved notes; the pattern keeps its own.
        let seq = p.sequence();
        assert_eq!(seq.patterns[0].notes[0].iter().map(|n| n.start).collect::<Vec<_>>(), [0, 36, 48, 84]);
        assert_eq!(p.patterns[0].notes[0][1].start, 24);
        assert!(p.commit_groove(0));
        assert!(p.patterns[0].groove.is_none());
        assert_eq!(p.patterns[0].notes[0].iter().map(|n| n.start).collect::<Vec<_>>(), [0, 36, 48, 84]);
        assert_eq!(p.extract_groove(0), Some(0));
        assert_eq!(p.grooves[0].timing[1], 12.0);
        assert_eq!(p.groove_pool().len(), SWINGS.len() + 1);
    }
}
