//! MIDI generators: Euclidean rhythms, drum rhythms by style and density,
//! melodies on the scale, chord progressions by mood and basslines that
//! follow the chords. All pure and seeded: the same settings and seed always
//! give the same notes.

use ponzik_engine::{Note, TICKS_PER_BAR, TICKS_PER_STEP};

use crate::scale::{ScaleKind, Voicing, revoice};
use crate::transform::Rng;

/// Euclidean rhythm E(`pulses`, `steps`) (Bjorklund's algorithm): the pulses
/// spread as evenly as possible, then rotated `rotation` steps later.
/// E(3, 8) is `x..x..x.`, E(5, 8) `x.xx.xx.`.
pub fn euclid(steps: usize, pulses: usize, rotation: usize) -> Vec<bool> {
    if steps == 0 {
        return Vec::new();
    }
    let pulses = pulses.min(steps);
    let mut a: Vec<Vec<bool>> = vec![vec![true]; pulses];
    let mut b: Vec<Vec<bool>> = vec![vec![false]; steps - pulses];
    if pulses == 0 {
        return vec![false; steps];
    }
    while b.len() > 1 {
        let n = a.len().min(b.len());
        let rest = if a.len() > n { a.split_off(n) } else { b.split_off(n) };
        for (x, y) in a.iter_mut().zip(b.drain(..)) {
            x.extend(y);
        }
        b = rest;
    }
    let pattern: Vec<bool> = a.into_iter().chain(b).flatten().collect();
    let r = rotation % steps;
    (0..steps).map(|i| pattern[(i + steps - r) % steps]).collect()
}

/// A Euclidean rhythm repeated over `len` ticks, one step every `step` ticks.
pub fn euclid_notes(len: u32, step: u32, steps: usize, pulses: usize, rotation: usize, key: u8, vel: f32) -> Vec<Note> {
    let hits = euclid(steps.max(1), pulses, rotation);
    let step = step.max(1);
    (0..len / step)
        .filter(|&i| hits[i as usize % hits.len()])
        .map(|i| {
            // The first pulse of each cycle is accented.
            let first = i as usize % hits.len() == hits.iter().position(|&h| h).unwrap_or(0);
            Note::new(i * step, step, key, if first { vel } else { vel * 0.82 })
        })
        .collect()
}

/// Drum rhythm style: how much each sixteenth of a bar wants a hit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RhythmStyle {
    /// Kick on the beats, pickups before them.
    #[default]
    Kick,
    /// Backbeat on 2 and 4, ghost notes around.
    Snare,
    /// Steady eighths, sixteenths in between.
    Hats,
    /// Trap hats: sixteenths with rolls (ratchets).
    Trap,
    /// Syncopated breakbeat kick.
    Break,
    /// Off-beats and clave-like accents.
    Percussion,
}

impl RhythmStyle {
    pub const ALL: [RhythmStyle; 6] = [Self::Kick, Self::Snare, Self::Hats, Self::Trap, Self::Break, Self::Percussion];

    /// Weight of each sixteenth of a bar, 0 to 1.
    fn weights(self) -> [f32; 16] {
        match self {
            Self::Kick => [1.0, 0.1, 0.2, 0.15, 0.85, 0.1, 0.25, 0.3, 0.9, 0.1, 0.35, 0.2, 0.85, 0.15, 0.3, 0.4],
            Self::Snare => [0.0, 0.1, 0.15, 0.2, 1.0, 0.1, 0.2, 0.3, 0.05, 0.2, 0.15, 0.25, 1.0, 0.2, 0.35, 0.45],
            Self::Hats => [0.95, 0.4, 0.9, 0.4, 0.95, 0.4, 0.9, 0.45, 0.95, 0.4, 0.9, 0.4, 0.95, 0.4, 0.9, 0.5],
            Self::Trap => [0.9, 0.6, 0.85, 0.6, 0.9, 0.6, 0.85, 0.65, 0.9, 0.6, 0.85, 0.6, 0.9, 0.6, 0.85, 0.7],
            Self::Break => [1.0, 0.1, 0.4, 0.2, 0.1, 0.2, 0.55, 0.3, 0.2, 0.15, 0.85, 0.2, 0.1, 0.3, 0.35, 0.2],
            Self::Percussion => [0.3, 0.2, 0.85, 0.3, 0.2, 0.3, 0.9, 0.2, 0.3, 0.2, 0.8, 0.3, 0.6, 0.3, 0.85, 0.4],
        }
    }
}

/// A drum rhythm over `len` ticks for one sound (`key`): `density` 0 to 1
/// sets how many steps play, the style where they fall.
pub fn rhythm(len: u32, style: RhythmStyle, density: f32, key: u8, seed: u64) -> Vec<Note> {
    let w = style.weights();
    let mut rng = Rng::new(seed);
    let density = density.clamp(0.0, 1.0);
    let mut out = Vec::new();
    for i in 0..len / TICKS_PER_STEP {
        let weight = w[i as usize % 16];
        let jitter = rng.signed() * 0.22;
        if weight + jitter <= 1.0 - density || density == 0.0 {
            continue;
        }
        let vel = (0.55 + 0.4 * weight + rng.signed() * 0.05).clamp(0.3, 1.0);
        let mut note = Note::new(i * TICKS_PER_STEP, TICKS_PER_STEP, key, vel);
        // Trap rolls: some off-beat hats burst into repeats.
        if style == RhythmStyle::Trap && i % 4 != 0 && rng.chance(0.18 + 0.2 * density) {
            note.ratchet = [2, 3, 4, 2][rng.below(4)];
        }
        out.push(note);
    }
    out
}

/// Overall shape of a generated melody.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Contour {
    Rising,
    Falling,
    #[default]
    Arch,
    Valley,
    Wave,
    /// A free random walk.
    Free,
}

impl Contour {
    pub const ALL: [Contour; 6] = [Self::Rising, Self::Falling, Self::Arch, Self::Valley, Self::Wave, Self::Free];

    /// Target height (0 to 1) at `x` (0 to 1) through the phrase.
    fn at(self, x: f32) -> Option<f32> {
        use std::f32::consts::PI;
        match self {
            Self::Rising => Some(x),
            Self::Falling => Some(1.0 - x),
            Self::Arch => Some((PI * x).sin()),
            Self::Valley => Some(1.0 - (PI * x).sin()),
            Self::Wave => Some(0.5 + 0.5 * (2.0 * PI * x * 2.0).sin()),
            Self::Free => None,
        }
    }
}

/// Scale used to write melodies and chords: the project's, or for the
/// chromatic scale (no scale chosen), the major scale of the key.
pub fn writing_scale(scale: ScaleKind) -> ScaleKind {
    if scale == ScaleKind::Chromatic { ScaleKind::Major } else { scale }
}

/// Notes of the scale from `low` to `high` included.
fn ladder(key: u8, scale: ScaleKind, low: u8, high: u8) -> Vec<u8> {
    (low..=high.min(127)).filter(|&k| scale.contains(key, k)).collect()
}

/// Settings of the melody generator.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MelodySpec {
    pub key: u8,
    pub scale: ScaleKind,
    /// Lowest note.
    pub low: u8,
    /// Range above it, in semitones.
    pub range: u8,
    /// How many steps get a note, 0 to 1.
    pub density: f32,
    pub contour: Contour,
}

/// A melody on the scale over `len` ticks, on a sixteenth grid.
pub fn melody(len: u32, spec: MelodySpec, seed: u64) -> Vec<Note> {
    let scale = writing_scale(spec.scale);
    let pool = ladder(spec.key, scale, spec.low, spec.low.saturating_add(spec.range.max(1)));
    let pool = if pool.is_empty() { vec![scale.snap(spec.key, spec.low)] } else { pool };
    let top = (pool.len() - 1) as f32;
    let mut rng = Rng::new(seed);
    let density = spec.density.clamp(0.05, 1.0);
    let steps = (len / TICKS_PER_STEP).max(1);
    let mut starts = Vec::new();
    for i in 0..steps {
        // Beats first, then eighths, then sixteenths.
        let pull = if i % 4 == 0 {
            1.0
        } else if i % 2 == 0 {
            0.6
        } else {
            0.3
        };
        if i == 0 || rng.chance(density * pull * 1.6) {
            starts.push(i * TICKS_PER_STEP);
        }
    }
    let mut out: Vec<Note> = Vec::with_capacity(starts.len());
    let mut at = rng.below(pool.len()) as f32;
    for (j, &start) in starts.iter().enumerate() {
        let x = start as f32 / len.max(1) as f32;
        let wanted = match spec.contour.at(x) {
            Some(h) => h * top,
            None => (at + rng.signed() * 2.5).clamp(0.0, top),
        };
        // Mostly steps towards the shape, sometimes a small leap.
        let leap = if rng.chance(0.2) { rng.signed() * 2.0 } else { rng.signed() * 0.7 };
        let pull = (wanted - at).clamp(-2.0, 2.0);
        at = (at + pull + leap).clamp(0.0, top);
        let key = pool[at.round() as usize];
        let next = starts.get(j + 1).copied().unwrap_or(len);
        let room = next - start;
        let note_len = if room > 2 * TICKS_PER_STEP && rng.chance(0.5) { 2 * TICKS_PER_STEP } else { room.min(4 * TICKS_PER_STEP) };
        let accent = if start % (4 * TICKS_PER_STEP) == 0 { 0.1 } else { 0.0 };
        out.push(Note::new(start, note_len.max(1), key, (0.72 + accent + rng.signed() * 0.06).clamp(0.3, 1.0)));
    }
    out
}

/// Mood of a chord progression.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mood {
    #[default]
    Uplifting,
    Melancholic,
    Epic,
    Dreamy,
    Dark,
    Jazzy,
}

impl Mood {
    pub const ALL: [Mood; 6] = [Self::Uplifting, Self::Melancholic, Self::Epic, Self::Dreamy, Self::Dark, Self::Jazzy];

    /// Common progressions, as degrees of the scale (0: the tonic).
    fn progressions(self) -> &'static [[usize; 4]] {
        match self {
            Self::Uplifting => &[[0, 4, 5, 3], [0, 5, 3, 4], [0, 3, 4, 3], [3, 4, 0, 0]],
            Self::Melancholic => &[[5, 3, 0, 4], [0, 5, 3, 4], [5, 4, 3, 4], [0, 2, 5, 3]],
            Self::Epic => &[[0, 5, 2, 6], [0, 6, 5, 6], [5, 3, 0, 4], [0, 3, 5, 4]],
            Self::Dreamy => &[[0, 3, 0, 3], [3, 0, 3, 4], [0, 2, 3, 3], [0, 5, 3, 3]],
            Self::Dark => &[[0, 5, 6, 0], [0, 1, 0, 6], [0, 3, 1, 0], [0, 5, 3, 6]],
            Self::Jazzy => &[[1, 4, 0, 0], [0, 5, 1, 4], [2, 5, 1, 4], [3, 2, 1, 0]],
        }
    }

    /// Notes per chord: sevenths for the moods that want color.
    fn size(self) -> usize {
        match self {
            Self::Dreamy | Self::Jazzy => 4,
            _ => 3,
        }
    }
}

/// Settings of the chord progression generator.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProgressionSpec {
    pub key: u8,
    pub scale: ScaleKind,
    pub mood: Mood,
    /// Length of each chord in ticks.
    pub chord_len: u32,
    /// Octave of the chords (4: around middle C).
    pub octave: i32,
}

/// Scale for diatonic chords: seven notes are needed, so other scales fall
/// back to the major or minor scale that shares their third.
fn harmony_scale(scale: ScaleKind) -> ScaleKind {
    let scale = writing_scale(scale);
    if scale.intervals().len() == 7 {
        scale
    } else if scale.intervals().contains(&3) && !scale.intervals().contains(&4) {
        ScaleKind::Minor
    } else {
        ScaleKind::Major
    }
}

/// The degrees a progression generator picks for `mood` with `seed`.
pub fn progression_degrees(mood: Mood, seed: u64) -> [usize; 4] {
    let list = mood.progressions();
    list[Rng::new(seed).below(list.len())]
}

/// A chord progression over `len` ticks: a progression of the mood (picked
/// by the seed), its chords in the scale, voiced close to one another.
pub fn progression(len: u32, spec: ProgressionSpec, seed: u64) -> Vec<Note> {
    let scale = harmony_scale(spec.scale);
    let degrees = progression_degrees(spec.mood, seed);
    let chord_len = spec.chord_len.max(TICKS_PER_STEP);
    let mut rng = Rng::new(seed ^ 0x5eed);
    let mut out = Vec::new();
    let mut center = 12.0 * (spec.octave + 1) as f32 + 7.0;
    for (i, start) in (0..len).step_by(chord_len as usize).enumerate() {
        let degree = degrees[i % degrees.len()];
        let base = scale.degree_chord(spec.key, degree, spec.mood.size(), spec.octave);
        // The inversion closest to the previous chord: smooth voice leading.
        let best = (0..base.len())
            .flat_map(|inv| [0i32, -12].map(|shift| (inv, shift)))
            .map(|(inv, shift)| {
                let v: Vec<u8> = revoice(&base, inv, Voicing::Close).iter().filter_map(|&k| u8::try_from(i32::from(k) + shift).ok()).collect();
                let mean = v.iter().map(|&k| k as f32).sum::<f32>() / v.len().max(1) as f32;
                ((mean - center).abs(), v)
            })
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, v)| v)
            .unwrap_or(base);
        center = center * 0.5 + best.iter().map(|&k| k as f32).sum::<f32>() / best.len().max(1) as f32 * 0.5;
        let vel = 0.72 + rng.signed() * 0.04;
        let l = chord_len.min(len - start);
        out.extend(best.into_iter().filter(|&k| k <= 127).map(|k| Note::new(start, l, k, vel)));
    }
    out.sort_by_key(|n| (n.start, n.key));
    out
}

/// A stretch of harmony: from `start` to `end` (ticks), a chord on `root`
/// (pitch class), with its third and fifth above the root in semitones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChordSpan {
    pub start: u32,
    pub end: u32,
    pub root: u8,
    pub third: u8,
    pub fifth: u8,
}

fn span_for(start: u32, notes: &[u8]) -> Option<ChordSpan> {
    let low = *notes.iter().min()?;
    let has = |i: u8| notes.iter().any(|&k| k >= low && (k - low) % 12 == i);
    let third = if has(4) {
        4
    } else if has(3) {
        3
    } else if has(5) {
        5
    } else if has(2) {
        2
    } else {
        4
    };
    let fifth = if has(7) {
        7
    } else if has(6) {
        6
    } else if has(8) {
        8
    } else {
        7
    };
    Some(ChordSpan { start, end: start, root: low % 12, third, fifth })
}

/// Reads the harmony of a part (a chord or keys channel): at each note start
/// the notes that sound make a chord on their lowest note. A chord lasts
/// until the next one with another root, the last one until `len`.
pub fn chords_from_notes(notes: &[Note], len: u32) -> Vec<ChordSpan> {
    let mut starts: Vec<u32> = notes.iter().map(|n| n.start).filter(|&s| s < len).collect();
    starts.sort_unstable();
    starts.dedup();
    let mut out: Vec<ChordSpan> = Vec::new();
    for t in starts {
        let sounding: Vec<u8> = notes.iter().filter(|n| n.start <= t && t < n.start + n.len.max(1)).map(|n| n.key).collect();
        let Some(span) = span_for(t, &sounding) else { continue };
        match out.last() {
            Some(last) if last.root == span.root => {}
            _ => out.push(span),
        }
    }
    for i in 0..out.len() {
        out[i].end = out.get(i + 1).map_or(len, |n| n.start);
    }
    out
}

/// The harmony of a progression of scale degrees, one chord every `chord_len` ticks.
pub fn chords_from_degrees(degrees: &[usize], key: u8, scale: ScaleKind, chord_len: u32, len: u32) -> Vec<ChordSpan> {
    let scale = harmony_scale(scale);
    let chord_len = chord_len.max(TICKS_PER_STEP);
    if degrees.is_empty() {
        return Vec::new();
    }
    (0..len)
        .step_by(chord_len as usize)
        .enumerate()
        .filter_map(|(i, start)| {
            let notes = scale.degree_chord(key, degrees[i % degrees.len()], 3, 4);
            span_for(start, &notes).map(|s| ChordSpan { end: (start + chord_len).min(len), ..s })
        })
        .collect()
}

/// Rhythm of a generated bassline.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BassRhythm {
    /// One long root per chord.
    Sustained,
    /// Driving eighths on the root.
    #[default]
    Eighths,
    /// Quarters alternating root and fifth.
    RootFifth,
    /// Eighths jumping between the root and its octave.
    Octaves,
    /// 3 + 3 + 2 syncopation.
    Syncopated,
    /// Walking quarters: root, third, fifth, then a note leading to the next chord.
    Walking,
}

impl BassRhythm {
    pub const ALL: [BassRhythm; 6] = [Self::Sustained, Self::Eighths, Self::RootFifth, Self::Octaves, Self::Syncopated, Self::Walking];
}

/// A bassline that follows `chords`, its roots in octave `octave` (2: from C1
/// to B1 is octave 1, so 2 starts at MIDI 36).
pub fn bassline(chords: &[ChordSpan], octave: i32, rhythm: BassRhythm, seed: u64) -> Vec<Note> {
    let mut rng = Rng::new(seed);
    let mut out = Vec::new();
    let eighth = TICKS_PER_STEP * 2;
    let quarter = TICKS_PER_STEP * 4;
    for (ci, c) in chords.iter().enumerate() {
        let root = (12 * (octave + 1) + i32::from(c.root)).clamp(0, 127) as u8;
        let up = |i: u8| root.saturating_add(i).min(127);
        let next_root = chords.get(ci + 1).map(|n| (12 * (octave + 1) + i32::from(n.root)).clamp(0, 127) as u8);
        let mut put = |start: u32, len: u32, key: u8, vel: f32| {
            if start < c.end {
                out.push(Note::new(start, len.min(c.end - start).max(1), key, vel));
            }
        };
        match rhythm {
            BassRhythm::Sustained => put(c.start, c.end - c.start, root, 0.85),
            BassRhythm::Eighths => {
                for (k, t) in (c.start..c.end).step_by(eighth as usize).enumerate() {
                    // Now and then an octave on an off-beat, for life.
                    let key = if k % 2 == 1 && rng.chance(0.15) { up(12) } else { root };
                    put(t, eighth - 6, key, if k % 2 == 0 { 0.9 } else { 0.75 });
                }
            }
            BassRhythm::RootFifth => {
                for (k, t) in (c.start..c.end).step_by(quarter as usize).enumerate() {
                    put(t, quarter - 8, if k % 2 == 0 { root } else { up(c.fifth) }, 0.85);
                }
            }
            BassRhythm::Octaves => {
                for (k, t) in (c.start..c.end).step_by(eighth as usize).enumerate() {
                    put(t, eighth - 8, if k % 2 == 0 { root } else { up(12) }, if k % 2 == 0 { 0.9 } else { 0.72 });
                }
            }
            BassRhythm::Syncopated => {
                for bar in (c.start..c.end).step_by(TICKS_PER_BAR as usize) {
                    for (k, (at, l)) in [(0, 3), (3, 3), (6, 2)].into_iter().enumerate() {
                        let key = if k == 2 && rng.chance(0.5) { up(c.fifth) } else { root };
                        put(bar + at * eighth, l * eighth - 8, key, if k == 0 { 0.92 } else { 0.8 });
                    }
                }
            }
            BassRhythm::Walking => {
                let beats: Vec<u32> = (c.start..c.end).step_by(quarter as usize).collect();
                let count = beats.len();
                for (k, &t) in beats.iter().enumerate() {
                    let key = if k + 1 == count && count > 1 {
                        // Leads by a semitone into the next root (or back to this one).
                        let target = next_root.unwrap_or(root);
                        if rng.chance(0.5) { target.saturating_sub(1) } else { target.saturating_add(1).min(127) }
                    } else {
                        match k % 4 {
                            0 => root,
                            1 => up(c.third),
                            2 => up(c.fifth),
                            _ => up(if rng.chance(0.5) { 12 } else { c.third }),
                        }
                    };
                    put(t, quarter - 6, key, if k % 2 == 0 { 0.88 } else { 0.78 });
                }
            }
        }
    }
    out.sort_by_key(|n| (n.start, n.key));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn xs(v: &[bool]) -> String {
        v.iter().map(|&b| if b { 'x' } else { '.' }).collect()
    }

    #[test]
    fn euclidean_rhythms() {
        assert_eq!(xs(&euclid(8, 3, 0)), "x..x..x.");
        assert_eq!(xs(&euclid(8, 5, 0)), "x.xx.xx.");
        assert_eq!(xs(&euclid(16, 4, 0)), "x...x...x...x...");
        assert_eq!(xs(&euclid(5, 2, 0)), "x.x..");
        assert_eq!(xs(&euclid(12, 5, 0)), "x..x.x..x.x.");
        assert_eq!(xs(&euclid(8, 3, 1)), ".x..x..x");
        assert_eq!(xs(&euclid(8, 3, 9)), ".x..x..x");
        assert_eq!(xs(&euclid(4, 0, 0)), "....");
        assert_eq!(xs(&euclid(4, 9, 0)), "xxxx");
        assert!(euclid(0, 3, 0).is_empty());
        for steps in 1..=16 {
            for pulses in 0..=steps {
                assert_eq!(euclid(steps, pulses, 0).iter().filter(|&&b| b).count(), pulses);
            }
        }
        let notes = euclid_notes(TICKS_PER_BAR, TICKS_PER_STEP, 8, 3, 0, 36, 0.9);
        assert_eq!(notes.iter().map(|n| n.start / TICKS_PER_STEP).collect::<Vec<_>>(), [0, 3, 6, 8, 11, 14]);
        assert!(notes.iter().all(|n| n.key == 36));
        assert_eq!(notes[0].vel, 0.9);
        assert!(notes[1].vel < 0.9);
    }

    #[test]
    fn rhythms_follow_density_and_seed() {
        let len = 4 * TICKS_PER_BAR;
        for style in RhythmStyle::ALL {
            let a = rhythm(len, style, 0.5, 36, 9);
            assert_eq!(a, rhythm(len, style, 0.5, 36, 9));
            assert!(rhythm(len, style, 0.0, 36, 9).is_empty());
            let sparse = rhythm(len, style, 0.2, 36, 9).len();
            let dense = rhythm(len, style, 0.9, 36, 9).len();
            assert!(sparse < dense, "{style:?}: {sparse} < {dense}");
            assert!(a.iter().all(|n| n.start % TICKS_PER_STEP == 0 && n.start < len && n.key == 36));
        }
        // The kick lands on the downbeat, the snare on the backbeat.
        let kick = rhythm(TICKS_PER_BAR, RhythmStyle::Kick, 0.4, 36, 1);
        assert!(kick.iter().any(|n| n.start == 0));
        let snare = rhythm(TICKS_PER_BAR, RhythmStyle::Snare, 0.4, 38, 1);
        assert!(snare.iter().any(|n| n.start == 4 * TICKS_PER_STEP));
        assert!(snare.iter().all(|n| n.start != 0));
        assert!((0..20).any(|s| rhythm(len, RhythmStyle::Trap, 0.8, 42, s).iter().any(|n| n.ratchet > 1)));
    }

    #[test]
    fn melodies_stay_in_scale_and_range() {
        for contour in Contour::ALL {
            for seed in 0..20 {
                let spec = MelodySpec { key: 9, scale: ScaleKind::Minor, low: 57, range: 12, density: 0.6, contour };
                let m = melody(2 * TICKS_PER_BAR, spec, seed);
                assert_eq!(m, melody(2 * TICKS_PER_BAR, spec, seed));
                assert!(!m.is_empty());
                assert_eq!(m[0].start, 0);
                assert!(m.iter().all(|n| ScaleKind::Minor.contains(9, n.key) && (57..=69).contains(&n.key)), "{m:?}");
                assert!(m.windows(2).all(|w| w[0].start + w[0].len <= w[1].start), "no overlaps");
                assert!(m.iter().all(|n| n.start + n.len <= 2 * TICKS_PER_BAR));
            }
        }
        let spec = MelodySpec { key: 0, scale: ScaleKind::Chromatic, low: 60, range: 24, density: 0.8, contour: Contour::Free };
        assert!(melody(TICKS_PER_BAR, spec, 1).iter().all(|n| ScaleKind::Major.contains(0, n.key)), "chromatic writes in major");
        assert_ne!(melody(TICKS_PER_BAR, spec, 1), melody(TICKS_PER_BAR, spec, 2));
        let rising = MelodySpec { contour: Contour::Rising, scale: ScaleKind::Major, ..spec };
        let r = melody(4 * TICKS_PER_BAR, rising, 5);
        let half = r.len() / 2;
        let mean = |v: &[Note]| v.iter().map(|n| n.key as f32).sum::<f32>() / v.len() as f32;
        assert!(mean(&r[..half]) < mean(&r[half..]));
    }

    #[test]
    fn progressions_are_in_key() {
        for mood in Mood::ALL {
            for scale in [ScaleKind::Major, ScaleKind::Minor, ScaleKind::Dorian, ScaleKind::PentatonicMinor, ScaleKind::Chromatic] {
                let spec = ProgressionSpec { key: 2, scale, mood, chord_len: TICKS_PER_BAR, octave: 4 };
                let p = progression(4 * TICKS_PER_BAR, spec, 3);
                assert_eq!(p, progression(4 * TICKS_PER_BAR, spec, 3));
                let hs = harmony_scale(scale);
                assert!(p.iter().all(|n| hs.contains(2, n.key)), "{mood:?} {scale:?}");
                let starts: Vec<u32> = p.iter().map(|n| n.start).collect();
                assert_eq!(starts.iter().filter(|&&s| s == 0).count(), mood.size());
                assert!(p.iter().all(|n| n.len == TICKS_PER_BAR && (40..=90).contains(&n.key)));
            }
        }
        // C major, uplifting: one of its progressions, built on the right roots.
        let spec = ProgressionSpec { key: 0, scale: ScaleKind::Major, mood: Mood::Uplifting, chord_len: TICKS_PER_BAR, octave: 4 };
        let degrees = progression_degrees(Mood::Uplifting, 0);
        let p = progression(4 * TICKS_PER_BAR, spec, 0);
        // Inversions move the lowest note: each bar holds the pitch classes of its triad.
        for (bar, &d) in degrees.iter().enumerate() {
            let mut pcs: Vec<u8> = p.iter().filter(|n| n.start == bar as u32 * TICKS_PER_BAR).map(|n| n.key % 12).collect();
            let mut triad: Vec<u8> = ScaleKind::Major.degree_chord(0, d, 3, 4).iter().map(|k| k % 12).collect();
            pcs.sort_unstable();
            triad.sort_unstable();
            assert_eq!(pcs, triad, "bar {bar}");
        }
    }

    #[test]
    fn harmony_is_read_from_a_part() {
        let bar = TICKS_PER_BAR;
        // C major for a bar, then A minor (first inversion: C E A) held with a gap.
        let notes = [
            Note::new(0, bar, 48, 0.8),
            Note::new(0, bar, 52, 0.8),
            Note::new(0, bar, 55, 0.8),
            Note::new(bar, bar / 2, 45, 0.8),
            Note::new(bar, bar / 2, 48, 0.8),
            Note::new(bar, bar / 2, 52, 0.8),
            // A melody note over the same chord does not split it.
            Note::new(bar + 96, 24, 72, 0.8),
        ];
        let chords = chords_from_notes(&notes, 2 * bar);
        assert_eq!(chords, [
            ChordSpan { start: 0, end: bar, root: 0, third: 4, fifth: 7 },
            ChordSpan { start: bar, end: 2 * bar, root: 9, third: 3, fifth: 7 },
        ]);
        let from_degrees = chords_from_degrees(&[0, 5], 0, ScaleKind::Major, bar, 2 * bar);
        assert_eq!(from_degrees, chords);
    }

    #[test]
    fn basslines_follow_the_chords() {
        let bar = TICKS_PER_BAR;
        let chords = chords_from_degrees(&[0, 4, 5, 3], 0, ScaleKind::Major, bar, 4 * bar);
        for rhythm in BassRhythm::ALL {
            let b = bassline(&chords, 2, rhythm, 7);
            assert_eq!(b, bassline(&chords, 2, rhythm, 7));
            assert!(!b.is_empty());
            for c in &chords {
                let first = b.iter().find(|n| n.start >= c.start).unwrap();
                assert_eq!(first.start, c.start, "{rhythm:?}");
                assert_eq!(first.key, 36 + c.root, "{rhythm:?}: root first");
                assert!(b.iter().filter(|n| n.start >= c.start && n.start < c.end).all(|n| n.start + n.len <= c.end));
            }
            assert!(b.iter().all(|n| (30..=60).contains(&n.key)));
        }
        let sustained = bassline(&chords, 2, BassRhythm::Sustained, 0);
        assert_eq!(sustained.iter().map(|n| n.key).collect::<Vec<_>>(), [36, 43, 45, 41]);
        let fifths = bassline(&chords, 2, BassRhythm::RootFifth, 0);
        assert_eq!(fifths.iter().take(4).map(|n| n.key).collect::<Vec<_>>(), [36, 43, 36, 43]);
        let walk = bassline(&chords, 2, BassRhythm::Walking, 0);
        assert_eq!(walk.iter().take(3).map(|n| n.key).collect::<Vec<_>>(), [36, 40, 43]);
        assert!([42, 44].contains(&walk[3].key), "leads into G");
    }
}
