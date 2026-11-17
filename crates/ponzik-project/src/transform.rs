//! MIDI transformations: pure functions that turn a set of notes (a piano roll
//! selection, or a whole channel) into new notes. Chances, conditions and
//! repeats of the notes go along with them.

use ponzik_engine::{Note, splitmix};

use crate::scale::ScaleKind;

/// Seeded random numbers (SplitMix64): the same seed always gives the same
/// sequence, on every machine.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        splitmix(self.0)
    }

    /// Uniform in [0, 1).
    pub fn unit(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Uniform in [−1, 1).
    pub fn signed(&mut self) -> f32 {
        self.unit() * 2.0 - 1.0
    }

    /// Uniform in 0..n (0 when `n` is 0).
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 { 0 } else { (self.next_u64() % n as u64) as usize }
    }

    /// True with probability `p`.
    pub fn chance(&mut self, p: f32) -> bool {
        self.unit() < p
    }
}

/// Order in which an arpeggio walks through a chord.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ArpDir {
    #[default]
    Up,
    Down,
    UpDown,
    Random,
}

impl ArpDir {
    pub const ALL: [ArpDir; 4] = [Self::Up, Self::Down, Self::UpDown, Self::Random];
}

/// First tick and end (exclusive) of a set of notes.
fn span(notes: &[Note]) -> Option<(u32, u32)> {
    let lo = notes.iter().map(|n| n.start).min()?;
    let hi = notes.iter().map(|n| n.start + n.len.max(1)).max()?;
    Some((lo, hi))
}

/// Notes grouped by start (chords), in time order, each group by pitch.
fn chords(notes: &[Note]) -> Vec<Vec<Note>> {
    let mut sorted = notes.to_vec();
    sorted.sort_by_key(|n| (n.start, n.key));
    let mut out: Vec<Vec<Note>> = Vec::new();
    for n in sorted {
        match out.last_mut() {
            Some(g) if g[0].start == n.start => g.push(n),
            _ => out.push(vec![n]),
        }
    }
    out
}

fn sorted(mut notes: Vec<Note>) -> Vec<Note> {
    notes.sort_by_key(|n| (n.start, n.key));
    notes
}

/// Each chord (notes starting together) becomes an arpeggio of notes `rate`
/// ticks long over the chord's length. Single notes are left alone.
pub fn arpeggiate(notes: &[Note], dir: ArpDir, rate: u32, seed: u64) -> Vec<Note> {
    let rate = rate.max(1);
    let mut rng = Rng::new(seed);
    let mut out = Vec::new();
    for group in chords(notes) {
        if group.len() < 2 {
            out.extend(group);
            continue;
        }
        let start = group[0].start;
        let end = group.iter().map(|n| n.start + n.len.max(1)).max().unwrap_or(start + rate);
        let n = group.len();
        let order: Vec<usize> = match dir {
            ArpDir::Up | ArpDir::Random => (0..n).collect(),
            ArpDir::Down => (0..n).rev().collect(),
            ArpDir::UpDown => (0..n).chain((1..n - 1).rev()).collect(),
        };
        let mut last = usize::MAX;
        let mut t = start;
        let mut k = 0;
        while t < end {
            let i = if dir == ArpDir::Random {
                // Never the same note twice in a row.
                let mut i = rng.below(n);
                if i == last {
                    i = (i + 1 + rng.below(n - 1)) % n;
                }
                i
            } else {
                order[k % order.len()]
            };
            last = i;
            out.push(Note { start: t, len: rate.min(end - t), ..group[i] });
            t += rate;
            k += 1;
        }
    }
    sorted(out)
}

/// Spreads the notes of each chord over `spread` ticks, from the lowest
/// (`up`) or the highest; their ends stay in place.
pub fn strum(notes: &[Note], spread: u32, up: bool) -> Vec<Note> {
    let mut out = Vec::new();
    for mut group in chords(notes) {
        if !up {
            group.reverse();
        }
        let n = group.len();
        for (i, note) in group.into_iter().enumerate() {
            let delay = if n > 1 { (spread as u64 * i as u64 / (n as u64 - 1)) as u32 } else { 0 };
            let delay = delay.min(note.len.saturating_sub(1));
            out.push(Note { start: note.start + delay, len: note.len - delay, ..note });
        }
    }
    sorted(out)
}

/// A grace note `offset` ticks before each note, at `vel` times its velocity.
/// A note too early for it is pushed back to make room.
pub fn flam(notes: &[Note], offset: u32, vel: f32) -> Vec<Note> {
    let offset = offset.max(1);
    let mut out = Vec::with_capacity(notes.len() * 2);
    for &n in notes {
        let main = if n.start >= offset { n } else { Note { start: n.start + offset, len: n.len.saturating_sub(offset).max(1), ..n } };
        let grace = Note { start: main.start - offset, len: offset, vel: (n.vel * vel).clamp(0.05, 1.0), ratchet: 1, ..n };
        out.push(grace);
        out.push(main);
    }
    sorted(out)
}

/// Each note lasts until the next note starts (the last ones keep their length).
pub fn legato(notes: &[Note]) -> Vec<Note> {
    let mut starts: Vec<u32> = notes.iter().map(|n| n.start).collect();
    starts.sort_unstable();
    starts.dedup();
    let out = notes
        .iter()
        .map(|&n| {
            let next = starts.partition_point(|&s| s <= n.start);
            match starts.get(next) {
                Some(&s) => Note { len: s - n.start, ..n },
                None => n,
            }
        })
        .collect();
    sorted(out)
}

/// Shortens every note to `ratio` of its length.
pub fn staccato(notes: &[Note], ratio: f32) -> Vec<Note> {
    let ratio = ratio.clamp(0.01, 1.0);
    sorted(notes.iter().map(|&n| Note { len: ((n.len as f32 * ratio).round() as u32).max(1), ..n }).collect())
}

/// Plays the notes backwards in time, over the span they cover.
pub fn reverse(notes: &[Note]) -> Vec<Note> {
    let Some((lo, hi)) = span(notes) else { return Vec::new() };
    sorted(notes.iter().map(|&n| Note { start: lo + hi - (n.start + n.len.max(1)), ..n }).collect())
}

/// Every MIDI note of a scale (chromatic: all of them), from low to high.
fn scale_ladder(key: u8, scale: ScaleKind) -> Vec<u8> {
    (0..=127u8).filter(|&k| scale.contains(key, k)).collect()
}

/// Mirrors the pitches around `center`: by semitones, or by steps of the
/// scale (`Some((key, scale))`), so the result stays in it.
pub fn invert(notes: &[Note], center: u8, scale: Option<(u8, ScaleKind)>) -> Vec<Note> {
    let out = match scale {
        Some((key, scale)) if scale != ScaleKind::Chromatic => {
            let ladder = scale_ladder(key, scale);
            let index = |k: u8| ladder.iter().position(|&s| s == scale.snap(key, k)).unwrap_or(0) as i32;
            let c = index(center);
            notes
                .iter()
                .map(|&n| {
                    let i = (2 * c - index(n.key)).clamp(0, ladder.len() as i32 - 1);
                    Note { key: ladder[i as usize], ..n }
                })
                .collect()
        }
        _ => notes.iter().map(|&n| Note { key: (2 * i32::from(center) - i32::from(n.key)).clamp(0, 127) as u8, ..n }).collect(),
    };
    sorted(out)
}

/// The pitch halfway between the lowest and the highest note.
pub fn middle_key(notes: &[Note]) -> u8 {
    let lo = notes.iter().map(|n| n.key).min().unwrap_or(60);
    let hi = notes.iter().map(|n| n.key).max().unwrap_or(60);
    ((u16::from(lo) + u16::from(hi)) / 2) as u8
}

/// Stretches (`factor` > 1) or compresses time from the first note.
pub fn stretch(notes: &[Note], factor: f32) -> Vec<Note> {
    let Some((lo, _)) = span(notes) else { return Vec::new() };
    let f = factor.clamp(1.0 / 64.0, 64.0) as f64;
    let out = notes
        .iter()
        .map(|&n| {
            let start = lo as f64 + (n.start - lo) as f64 * f;
            Note { start: start.round() as u32, len: ((n.len as f64 * f).round() as u32).max(1), ..n }
        })
        .collect();
    sorted(out)
}

/// Merges notes of the same pitch that overlap or touch into one.
pub fn glue(notes: &[Note]) -> Vec<Note> {
    let mut by_key = notes.to_vec();
    by_key.sort_by_key(|n| (n.key, n.start));
    let mut out: Vec<Note> = Vec::with_capacity(by_key.len());
    for n in by_key {
        match out.last_mut() {
            Some(last) if last.key == n.key && n.start <= last.start + last.len => {
                last.len = last.len.max(n.start + n.len - last.start);
            }
            _ => out.push(n),
        }
    }
    sorted(out)
}

/// Pulls each start towards the `grid` (ticks) by `strength` (0 to 1); `swing`
/// (0 to 1) delays every other grid line by up to half a grid.
pub fn quantize(notes: &[Note], grid: u32, strength: f32, swing: f32) -> Vec<Note> {
    let grid = grid.max(1);
    let strength = strength.clamp(0.0, 1.0);
    let swing = swing.clamp(0.0, 1.0);
    let out = notes
        .iter()
        .map(|&n| {
            let line = (n.start + grid / 2) / grid;
            let mut target = (line * grid) as f32;
            if line % 2 == 1 {
                target += swing * grid as f32 * 0.5;
            }
            let start = n.start as f32 + (target - n.start as f32) * strength;
            Note { start: start.round().max(0.0) as u32, ..n }
        })
        .collect();
    sorted(out)
}

/// Shifts each start by up to ±`ticks` and each velocity by up to ±`vel`
/// (relative), at random.
pub fn humanize(notes: &[Note], ticks: u32, vel: f32, seed: u64) -> Vec<Note> {
    let mut rng = Rng::new(seed);
    let out = notes
        .iter()
        .map(|&n| {
            let (dt, dv) = (rng.signed() * ticks as f32, rng.signed() * vel);
            Note { start: (n.start as f32 + dt).round().max(0.0) as u32, vel: (n.vel * (1.0 + dv)).clamp(0.05, 1.0), ..n }
        })
        .collect();
    sorted(out)
}

/// Moves every note outside the scale to its nearest note in it.
pub fn scale_correct(notes: &[Note], key: u8, scale: ScaleKind) -> Vec<Note> {
    sorted(notes.iter().map(|&n| Note { key: scale.snap(key, n.key), ..n }).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ponzik_engine::NoteCond;

    fn n(start: u32, len: u32, key: u8) -> Note {
        Note::new(start, len, key, 0.8)
    }

    fn sk(notes: &[Note]) -> Vec<(u32, u32, u8)> {
        notes.iter().map(|n| (n.start, n.len, n.key)).collect()
    }

    fn chord_c() -> Vec<Note> {
        vec![n(0, 96, 60), n(0, 96, 64), n(0, 96, 67)]
    }

    #[test]
    fn rng_is_seeded() {
        let a: Vec<u64> = { let mut r = Rng::new(7); (0..5).map(|_| r.next_u64()).collect() };
        let b: Vec<u64> = { let mut r = Rng::new(7); (0..5).map(|_| r.next_u64()).collect() };
        assert_eq!(a, b);
        let mut r = Rng::new(1);
        assert!((0..1000).all(|_| (0.0..1.0).contains(&r.unit()) && r.below(5) < 5));
    }

    #[test]
    fn arpeggios() {
        let up = arpeggiate(&chord_c(), ArpDir::Up, 24, 0);
        assert_eq!(sk(&up), [(0, 24, 60), (24, 24, 64), (48, 24, 67), (72, 24, 60)]);
        let down = arpeggiate(&chord_c(), ArpDir::Down, 24, 0);
        assert_eq!(down.iter().map(|n| n.key).collect::<Vec<_>>(), [67, 64, 60, 67]);
        let ud = arpeggiate(&chord_c(), ArpDir::UpDown, 12, 0);
        assert_eq!(ud.iter().map(|n| n.key).collect::<Vec<_>>(), [60, 64, 67, 64, 60, 64, 67, 64]);
        let random = arpeggiate(&chord_c(), ArpDir::Random, 12, 3);
        assert_eq!(random.len(), 8);
        assert!(random.windows(2).all(|w| w[0].key != w[1].key));
        assert_eq!(random, arpeggiate(&chord_c(), ArpDir::Random, 12, 3));
        // The last step is cut at the chord's end; a lone note stays.
        let odd = arpeggiate(&[n(0, 40, 60), n(0, 40, 64), n(96, 24, 50)], ArpDir::Up, 24, 0);
        assert_eq!(sk(&odd), [(0, 24, 60), (24, 16, 64), (96, 24, 50)]);
    }

    #[test]
    fn strums_keep_the_ends() {
        let up = strum(&chord_c(), 12, true);
        assert_eq!(sk(&up), [(0, 96, 60), (6, 90, 64), (12, 84, 67)]);
        let down = strum(&chord_c(), 12, false);
        assert_eq!(sk(&down), [(0, 96, 67), (6, 90, 64), (12, 84, 60)]);
    }

    #[test]
    fn flams_add_grace_notes() {
        let out = flam(&[n(96, 24, 38), n(0, 24, 36)], 6, 0.5);
        assert_eq!(sk(&out), [(0, 6, 36), (6, 18, 36), (90, 6, 38), (96, 24, 38)]);
        assert_eq!(out[2].vel, 0.4);
        assert_eq!(out[3].vel, 0.8);
    }

    #[test]
    fn legato_and_staccato() {
        let notes = [n(0, 10, 60), n(0, 10, 64), n(48, 10, 62), n(96, 30, 65)];
        assert_eq!(sk(&legato(&notes)), [(0, 48, 60), (0, 48, 64), (48, 48, 62), (96, 30, 65)]);
        assert_eq!(sk(&staccato(&notes, 0.5)), [(0, 5, 60), (0, 5, 64), (48, 5, 62), (96, 15, 65)]);
        assert_eq!(staccato(&[n(0, 1, 60)], 0.1)[0].len, 1);
    }

    #[test]
    fn reverse_mirrors_time() {
        let out = reverse(&[n(96, 24, 60), n(120, 72, 62), n(192, 24, 64)]);
        assert_eq!(sk(&out), [(96, 24, 64), (120, 72, 62), (192, 24, 60)]);
    }

    #[test]
    fn inversions() {
        let notes = [n(0, 24, 60), n(24, 24, 64), n(48, 24, 67)];
        assert_eq!(invert(&notes, 64, None).iter().map(|n| n.key).collect::<Vec<_>>(), [68, 64, 61]);
        // In C major around E: C (2 steps below) becomes G, G (2 steps above) becomes C.
        let out = invert(&notes, 64, Some((0, ScaleKind::Major)));
        assert_eq!(out.iter().map(|n| n.key).collect::<Vec<_>>(), [67, 64, 60]);
        assert!(out.iter().all(|n| ScaleKind::Major.contains(0, n.key)));
        assert_eq!(middle_key(&notes), 63);
        assert_eq!(invert(&[n(0, 1, 120)], 10, None)[0].key, 0, "clamped");
    }

    #[test]
    fn stretch_and_compress() {
        let notes = [n(96, 24, 60), n(120, 24, 62)];
        assert_eq!(sk(&stretch(&notes, 2.0)), [(96, 48, 60), (144, 48, 62)]);
        assert_eq!(sk(&stretch(&notes, 0.5)), [(96, 12, 60), (108, 12, 62)]);
        assert!(stretch(&[], 2.0).is_empty());
    }

    #[test]
    fn glue_merges_same_pitch() {
        let notes = [n(0, 24, 60), n(24, 24, 60), n(40, 30, 60), n(96, 24, 60), n(10, 5, 62)];
        assert_eq!(sk(&glue(&notes)), [(0, 70, 60), (10, 5, 62), (96, 24, 60)]);
    }

    #[test]
    fn quantize_strength_and_swing() {
        let notes = [n(5, 24, 60), n(30, 24, 62), n(70, 24, 64)];
        assert_eq!(sk(&quantize(&notes, 24, 1.0, 0.0)).iter().map(|x| x.0).collect::<Vec<_>>(), [0, 24, 72]);
        assert_eq!(quantize(&notes, 24, 0.5, 0.0).iter().map(|x| x.start).collect::<Vec<_>>(), [3, 27, 71]);
        // Swing pushes the odd grid lines (24 → 30, 72 → 78) at full strength.
        assert_eq!(quantize(&notes, 24, 1.0, 0.5).iter().map(|x| x.start).collect::<Vec<_>>(), [0, 30, 78]);
        assert_eq!(quantize(&notes, 24, 0.0, 1.0).iter().map(|x| x.start).collect::<Vec<_>>(), [5, 30, 70]);
    }

    #[test]
    fn humanize_is_seeded_and_bounded() {
        let notes: Vec<Note> = (0..64).map(|i| n(100 + i * 24, 24, 60)).collect();
        let a = humanize(&notes, 6, 0.1, 42);
        assert_eq!(a, humanize(&notes, 6, 0.1, 42));
        assert_ne!(a, humanize(&notes, 6, 0.1, 43));
        let mut orig = notes.clone();
        orig.sort_by_key(|n| n.start);
        for (h, o) in a.iter().zip(&orig) {
            assert!(h.start.abs_diff(o.start) <= 6);
            assert!((h.vel - o.vel).abs() <= 0.08 + 1e-6);
        }
    }

    #[test]
    fn scale_correction() {
        let notes = [n(0, 24, 61), n(24, 24, 63), n(48, 24, 64)];
        let out = scale_correct(&notes, 0, ScaleKind::Major);
        assert_eq!(out.iter().map(|n| n.key).collect::<Vec<_>>(), [60, 62, 64]);
    }

    #[test]
    fn note_settings_survive() {
        let varied = Note { prob: 40, cond: NoteCond::Every(1, 2), ratchet: 3, ..n(0, 96, 60) };
        for out in [stretch(&[varied], 2.0), reverse(&[varied]), quantize(&[varied], 24, 1.0, 0.0), legato(&[varied])] {
            assert_eq!((out[0].prob, out[0].cond, out[0].ratchet), (40, NoteCond::Every(1, 2), 3));
        }
    }
}
