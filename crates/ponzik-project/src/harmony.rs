//! Key and chord detection.
//!
//! The key comes from a pitch-class profile (how long each of the twelve
//! pitch classes sounds, from notes or from an audio chromagram) correlated
//! with the Krumhansl–Kessler major and minor profiles, the
//! Krumhansl–Schmuckler algorithm. Chords come from the pitch classes that
//! sound in a window, matched against [`ChordKind`]'s shapes.

use ponzik_engine::{Note, PPQ};

use crate::Project;
use crate::scale::{ChordKind, ScaleKind};

/// Krumhansl–Kessler probe-tone ratings of the major key, from the tonic.
const MAJOR_PROFILE: [f32; 12] = [6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88];
/// The same for the minor key.
const MINOR_PROFILE: [f32; 12] = [6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17];

/// A key candidate: tonic (0 = C), major or minor, and its correlation with
/// the profile, in [−1, 1].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct KeyGuess {
    pub key: u8,
    pub scale: ScaleKind,
    pub score: f32,
}

/// Pearson correlation of two profiles.
fn correlation(a: &[f32; 12], b: &[f32; 12]) -> f32 {
    let (ma, mb) = (a.iter().sum::<f32>() / 12.0, b.iter().sum::<f32>() / 12.0);
    let (mut num, mut da, mut db) = (0.0, 0.0, 0.0);
    for i in 0..12 {
        let (x, y) = (a[i] - ma, b[i] - mb);
        num += x * y;
        da += x * x;
        db += y * y;
    }
    if da <= 0.0 || db <= 0.0 { 0.0 } else { num / (da * db).sqrt() }
}

/// The 24 major and minor keys, best first; empty for a silent profile.
pub fn detect_key(profile: &[f32; 12]) -> Vec<KeyGuess> {
    if profile.iter().all(|&v| v <= 0.0 || !v.is_finite()) {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(24);
    for key in 0..12u8 {
        for (scale, base) in [(ScaleKind::Major, &MAJOR_PROFILE), (ScaleKind::Minor, &MINOR_PROFILE)] {
            let rotated: [f32; 12] = std::array::from_fn(|pc| base[(pc + 12 - key as usize) % 12]);
            out.push(KeyGuess { key, scale, score: correlation(profile, &rotated) });
        }
    }
    out.sort_by(|a, b| b.score.total_cmp(&a.score));
    out
}

/// How long each pitch class sounds in `notes` (ticks, louder notes count a
/// little more).
pub fn note_profile<'a>(notes: impl IntoIterator<Item = &'a Note>) -> [f32; 12] {
    let mut p = [0.0; 12];
    for n in notes.into_iter().filter(|n| !n.slide) {
        p[usize::from(n.key % 12)] += n.len.max(1) as f32 * (0.5 + 0.5 * n.vel.clamp(0.0, 1.0));
    }
    p
}

/// Pitch-class profile of the part of `sample` from `offset` seconds over
/// `secs` seconds (the whole rest of the file when `None`), mono mix.
pub fn audio_profile(sample: &ponzik_engine::Sample, offset: f64, secs: Option<f64>) -> [f32; 12] {
    let sr = sample.sample_rate.max(1.0) as f64;
    let a = ((offset.max(0.0) * sr) as usize).min(sample.len());
    let b = secs.map_or(sample.len(), |s| ((offset.max(0.0) + s.max(0.0)) * sr) as usize).clamp(a, sample.len());
    let mono = ponzik_dsp::onset::mono(&sample.left[a..b], &sample.right[a..b]);
    ponzik_dsp::chroma::chromagram(&mono, sample.sample_rate)
}

/// Bit set of pitch classes.
pub type PitchSet = u16;

/// Bit set of the pitch classes of a chord kind, from its root.
pub fn chord_set(kind: ChordKind) -> PitchSet {
    kind.intervals().iter().fold(0, |m, &i| m | 1 << (i % 12))
}

fn rotate(set: PitchSet, by: u8) -> PitchSet {
    let by = u32::from(by % 12);
    ((u32::from(set) >> by | u32::from(set) << (12 - by)) & 0xFFF) as PitchSet
}

/// A chord: root and bass pitch classes, kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChordName {
    pub root: u8,
    pub kind: ChordKind,
    /// Lowest pitch class, when it is not the root (a slash chord).
    pub bass: Option<u8>,
}

impl ChordName {
    /// "Am", "C/E", "G7", with the names given by `name` for pitch classes.
    pub fn label(&self, name: impl Fn(u8) -> &'static str) -> String {
        let mut s = format!("{}{}", name(self.root), self.kind.symbol());
        if let Some(b) = self.bass {
            s.push('/');
            s.push_str(name(b));
        }
        s
    }
}

/// Names the pitch classes `set` with `bass` the lowest one: an exact chord
/// shape, the bass as root first; else, with four notes or more, the shape
/// left once a passing note is dropped.
pub fn identify_set(set: PitchSet, bass: u8) -> Option<ChordName> {
    let bass = bass % 12;
    if set.count_ones() < 2 {
        return None;
    }
    let exact = |set: PitchSet| {
        let roots = std::iter::once(bass).chain((0..12).filter(|&r| r != bass));
        for root in roots.filter(|&r| set & 1 << r != 0) {
            let rel = rotate(set, root);
            if let Some(kind) = ChordKind::ALL.into_iter().find(|&k| chord_set(k) == rel) {
                return Some(ChordName { root, kind, bass: (root != bass).then_some(bass) });
            }
        }
        None
    };
    if let Some(c) = exact(set) {
        return Some(c);
    }
    if set.count_ones() >= 4 {
        for drop in (0..12u8).filter(|&p| p != bass && set & 1 << p != 0) {
            if let Some(c) = exact(set & !(1 << drop)) {
                return Some(c);
            }
        }
    }
    None
}

/// A chord over a span of ticks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChordSpan {
    pub start: u32,
    pub len: u32,
    pub chord: ChordName,
}

/// Chords of `notes` window by window over [0, `len`) (`window` ticks, a
/// beat for instance): the pitch classes that sound for at least a quarter
/// of a window, the lowest note as bass. The same chord on following
/// windows is one span.
pub fn chord_track<'a>(notes: impl IntoIterator<Item = &'a Note> + Clone, len: u32, window: u32) -> Vec<ChordSpan> {
    let window = window.max(1);
    let mut out: Vec<ChordSpan> = Vec::new();
    let mut start = 0;
    while start < len {
        let end = (start + window).min(len);
        let mut set: PitchSet = 0;
        let mut bass = u8::MAX;
        for n in notes.clone().into_iter().filter(|n| !n.slide) {
            let overlap = (n.start + n.len).min(end).saturating_sub(n.start.max(start));
            if overlap * 4 >= (end - start) {
                set |= 1 << (n.key % 12);
                bass = bass.min(n.key);
            }
        }
        if let Some(chord) = (bass < u8::MAX).then(|| identify_set(set, bass)).flatten() {
            match out.last_mut() {
                Some(last) if last.chord == chord && last.start + last.len == start => last.len += end - start,
                _ => out.push(ChordSpan { start, len: end - start, chord }),
            }
        }
        start = end;
    }
    out
}

impl Project {
    /// Pitch-class profile of the notes of pattern `pattern` (`None`: every
    /// pattern, weighted by how often the playlist plays it), on the channels
    /// `melodic` accepts.
    pub fn key_profile(&self, pattern: Option<usize>, melodic: impl Fn(usize) -> bool) -> [f32; 12] {
        let mut p = [0.0; 12];
        for (i, pat) in self.patterns.iter().enumerate() {
            let weight = match pattern {
                Some(only) if only != i => continue,
                Some(_) => 1.0,
                None => 1.0 + self.clips.iter().filter(|c| c.pattern == i && !c.muted).count() as f32,
            };
            for (_, notes) in pat.notes.iter().enumerate().filter(|(ch, _)| melodic(*ch)) {
                for (k, v) in note_profile(notes).iter().enumerate() {
                    p[k] += v * weight;
                }
            }
        }
        p
    }

    /// Chords of pattern `pattern` beat by beat, on the channels `melodic`
    /// accepts (`only`: those notes instead, the piano roll's selection).
    pub fn pattern_chords(&self, pattern: usize, melodic: impl Fn(usize) -> bool, only: Option<&[Note]>) -> Vec<ChordSpan> {
        let Some(pat) = self.patterns.get(pattern) else { return Vec::new() };
        let all: Vec<Note>;
        let notes: &[Note] = match only {
            Some(n) => n,
            None => {
                all = pat.notes.iter().enumerate().filter(|(ch, _)| melodic(*ch)).flat_map(|(_, n)| n.iter().copied()).collect();
                &all
            }
        };
        chord_track(notes, pat.len, PPQ)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notes(keys: &[(u8, u32, u32)]) -> Vec<Note> {
        keys.iter().map(|&(key, start, len)| Note::new(start, len, key, 0.8)).collect()
    }

    fn best(p: &[f32; 12]) -> (u8, ScaleKind) {
        let g = detect_key(p)[0];
        (g.key, g.scale)
    }

    #[test]
    fn keys_of_scales_and_progressions() {
        // C major scale, the tonic held longer.
        let c = notes(&[(60, 0, 192), (62, 192, 96), (64, 288, 96), (65, 384, 96), (67, 480, 96), (69, 576, 96), (71, 672, 96), (72, 768, 192)]);
        assert_eq!(best(&note_profile(&c)), (0, ScaleKind::Major));
        // A harmonic minor: the G♯ tells it from C major.
        let a = notes(&[(57, 0, 192), (59, 192, 96), (60, 288, 96), (62, 384, 96), (64, 480, 192), (65, 672, 96), (68, 768, 96), (69, 864, 192)]);
        assert_eq!(best(&note_profile(&a)), (9, ScaleKind::Minor));
        // I–IV–V–I in G, block chords.
        let mut g = Vec::new();
        for (i, chord) in [[55, 59, 62], [60, 64, 67], [62, 66, 69], [55, 59, 62]].iter().enumerate() {
            g.extend(chord.iter().map(|&k| Note::new(i as u32 * 384, 384, k, 0.8)));
        }
        assert_eq!(best(&note_profile(&g)), (7, ScaleKind::Major));
        // i–iv–V–i in D minor (A major chord: C♯).
        let mut d = Vec::new();
        for (i, chord) in [[50, 53, 57], [55, 58, 62], [57, 61, 64], [50, 53, 57]].iter().enumerate() {
            d.extend(chord.iter().map(|&k| Note::new(i as u32 * 384, 384, k, 0.8)));
        }
        assert_eq!(best(&note_profile(&d)), (2, ScaleKind::Minor));
        assert!(detect_key(&[0.0; 12]).is_empty());
        // The 24 keys, each once.
        assert_eq!(detect_key(&note_profile(&c)).len(), 24);
    }

    #[test]
    fn key_of_a_recording() {
        // i–iv–V–i in A minor (E major: G♯), as sines with two harmonics.
        let sr = 22_050.0;
        let mut left = Vec::new();
        for chord in [[57u8, 60, 64], [62, 65, 69], [64, 68, 71], [57, 60, 64]] {
            for i in 0..(sr as usize / 2) {
                let t = i as f32 / sr;
                let mut s = 0.0;
                for &k in &chord {
                    let hz = 440.0 * ((k as f32 - 69.0) / 12.0).exp2();
                    s += (std::f32::consts::TAU * hz * t).sin() + 0.3 * (std::f32::consts::TAU * 2.0 * hz * t).sin();
                }
                left.push(0.1 * s);
            }
        }
        let sample = ponzik_engine::Sample { name: "am".into(), sample_rate: sr, right: left.clone(), left, markers: Vec::new() };
        let g = detect_key(&audio_profile(&sample, 0.0, None))[0];
        assert_eq!((g.key, g.scale), (9, ScaleKind::Minor), "{g:?}");
        // Only the E major chord: E major.
        let e = detect_key(&audio_profile(&sample, 1.0, Some(0.5)))[0];
        assert_eq!(e.key, 4, "{e:?}");
    }

    #[test]
    fn chords_are_named_with_inversions() {
        let name = |keys: &[u8]| {
            let set = keys.iter().fold(0, |m, &k| m | 1 << (k % 12));
            identify_set(set, *keys.iter().min().unwrap()).map(|c| c.label(|pc| crate::scale::KEY_NAMES[pc as usize]))
        };
        assert_eq!(name(&[57, 60, 64]).as_deref(), Some("Am"));
        assert_eq!(name(&[64, 67, 72]).as_deref(), Some("C/E"));
        assert_eq!(name(&[55, 59, 62, 65]).as_deref(), Some("G7"));
        assert_eq!(name(&[60, 64, 67, 71]).as_deref(), Some("Cmaj7"));
        assert_eq!(name(&[62, 65, 69, 72]).as_deref(), Some("Dm7"));
        assert_eq!(name(&[48, 55]).as_deref(), Some("C5"));
        assert_eq!(name(&[60, 65, 67]).as_deref(), Some("Csus4"));
        // A passing note over a triad: still the triad.
        assert_eq!(name(&[60, 64, 66, 67]).as_deref(), Some("C"));
        assert_eq!(name(&[60, 61]), None);
        assert_eq!(name(&[60]), None);
    }

    #[test]
    fn chord_track_follows_the_beats() {
        // Am for two beats, F for one, then G with a passing E on the first half.
        let mut n = notes(&[(57, 0, 192), (60, 0, 192), (64, 0, 192), (53, 192, 96), (57, 192, 96), (60, 192, 96)]);
        n.extend(notes(&[(55, 288, 96), (59, 288, 96), (62, 288, 96), (64, 288, 12)]));
        let spans = chord_track(&n, 384, PPQ);
        let labels: Vec<(u32, u32, String)> =
            spans.iter().map(|s| (s.start, s.len, s.chord.label(|pc| crate::scale::KEY_NAMES[pc as usize]))).collect();
        assert_eq!(labels, [(0, 192, "Am".to_string()), (192, 96, "F".into()), (288, 96, "G".into())]);
    }
}
