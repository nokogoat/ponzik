//! Note-based pitch and timing editing of recorded audio (Melodyne, FL
//! Newtone, Auto-Tune's Graph mode), offline.
//!
//! [`analyze`] runs the Pitch Correction's voice detector (YIN every 2 ms,
//! octave checks, voicing from clarity, level and zero crossings) over a whole
//! file, cleans the track up and cuts it into notes ([`segment`]): stable
//! pitch regions split at pitch jumps, re-attacks and unvoiced gaps, each with
//! its centre pitch, vibrato and level. The drift of a note (its slow motion
//! around the centre) and its vibrato (the fast one) are read from the track
//! when needed ([`Analysis::note_curves`]).
//!
//! Edits are a list of [`NoteEdit`]s: where a note plays (time), the centre it
//! is moved to, how much of its drift and vibrato it keeps, its formants and
//! its level. [`render`] resynthesizes only the regions that differ from the
//! original (or from a previous render) with an offline pitch-synchronous
//! overlap-add that also stretches time locally, and crossfades them into an
//! untouched copy: what was not edited stays bit for bit the original.
//!
//! Everything here allocates: it runs on worker threads, never on the audio
//! thread. The playlist plays the rendered file.

mod analyze;
mod render;
mod segment;

#[cfg(test)]
mod tests;

pub use analyze::{analyze, analyze_with};
pub use render::{TimeMap, render, render_regions};

/// Time between two frames of the pitch track, in seconds.
pub const HOP: f64 = 0.002;
/// Bumped whenever the analysis changes: older caches are recomputed.
pub const ANALYSIS_VERSION: u32 = 1;

/// One detected note, in seconds of the file.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Note {
    pub start: f64,
    pub end: f64,
    /// Centre pitch (MIDI, fractional).
    pub centre: f32,
    /// Vibrato depth: peak deviation around the drift curve, in cents.
    pub vibrato: f32,
    /// Peak level, dBFS.
    pub level: f32,
}

/// The pitch track and notes of a file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Analysis {
    pub sample_rate: f32,
    /// Samples of the analysed file.
    pub len: usize,
    /// Pitch every [`HOP`] seconds (frame `k` at `k * HOP`), MIDI; NaN where unvoiced.
    pub pitch: Vec<f32>,
    /// Level every [`HOP`] seconds, dBFS (RMS over 10 ms).
    pub level: Vec<f32>,
    pub notes: Vec<Note>,
}

/// How long the smoothing that separates drift from vibrato is (one cycle of a 5.5 Hz vibrato).
const DRIFT_WINDOW: f64 = 0.18;

/// A note's pitch split in parts, frame by frame from its first frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NoteCurves {
    /// First frame of the note.
    pub first: usize,
    /// Pitch (MIDI, NaN unvoiced).
    pub pitch: Vec<f32>,
    /// Slow part around the centre (semitones, 0 where unvoiced).
    pub drift: Vec<f32>,
    /// Fast part (semitones, 0 where unvoiced).
    pub vibrato: Vec<f32>,
}

impl Analysis {
    pub fn duration(&self) -> f64 {
        self.len as f64 / f64::from(self.sample_rate.max(1.0))
    }

    /// Frame at or just before `secs`.
    pub fn frame_at(&self, secs: f64) -> usize {
        ((secs / HOP).floor().max(0.0) as usize).min(self.pitch.len().saturating_sub(1))
    }

    /// Pitch at `secs`, linear between frames (NaN unvoiced).
    pub fn pitch_at(&self, secs: f64) -> f32 {
        interp(&self.pitch, secs / HOP)
    }

    /// Frames `[a, b)` of a span of seconds.
    pub fn frames_of(&self, start: f64, end: f64) -> (usize, usize) {
        let n = self.pitch.len();
        let a = ((start / HOP).ceil().max(0.0) as usize).min(n);
        let b = ((end / HOP).ceil().max(0.0) as usize).clamp(a, n);
        (a, b)
    }

    /// Centre pitch, vibrato and level of the span `[start, end)` (a note cut
    /// or joined in the editor): `None` with no voiced frame.
    pub fn stats(&self, start: f64, end: f64) -> Option<Note> {
        let (a, b) = self.frames_of(start, end);
        segment::note_stats(self, a, b).map(|(centre, vibrato, level)| Note { start, end, centre, vibrato, level })
    }

    /// Drift and vibrato of the span `[start, end)`, around `centre`.
    pub fn note_curves(&self, start: f64, end: f64, centre: f32) -> NoteCurves {
        let (a, b) = self.frames_of(start, end);
        let pitch = self.pitch[a..b].to_vec();
        let slow = smooth_voiced(&pitch, (DRIFT_WINDOW / HOP / 2.0).round() as usize);
        // A note too short for a cycle of vibrato has none: all its motion is
        // drift (fully from 0.2 s down, the split as is from two cycles up).
        let len = (b - a) as f64 * HOP;
        let keep = ((len - 0.2) / (2.0 * DRIFT_WINDOW - 0.2)).clamp(0.0, 1.0) as f32;
        let slow: Vec<f32> = pitch.iter().zip(&slow).map(|(p, s)| if p.is_finite() { p + (s - p) * keep } else { *s }).collect();
        let drift = slow.iter().map(|s| if s.is_finite() { s - centre } else { 0.0 }).collect();
        let vibrato = pitch.iter().zip(&slow).map(|(p, s)| if p.is_finite() && s.is_finite() { p - s } else { 0.0 }).collect();
        NoteCurves { first: a, pitch, drift, vibrato }
    }

    /// Serialized form (the cache file).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(32 + 8 * self.pitch.len() + 28 * self.notes.len());
        out.extend_from_slice(b"PZPA");
        out.extend_from_slice(&ANALYSIS_VERSION.to_le_bytes());
        out.extend_from_slice(&self.sample_rate.to_le_bytes());
        out.extend_from_slice(&(self.len as u64).to_le_bytes());
        out.extend_from_slice(&(self.pitch.len() as u32).to_le_bytes());
        for v in self.pitch.iter().chain(&self.level) {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&(self.notes.len() as u32).to_le_bytes());
        for n in &self.notes {
            out.extend_from_slice(&n.start.to_le_bytes());
            out.extend_from_slice(&n.end.to_le_bytes());
            for v in [n.centre, n.vibrato, n.level] {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        out
    }

    /// Reads [`Analysis::to_bytes`]; `None` for another version or a broken file.
    pub fn from_bytes(b: &[u8]) -> Option<Analysis> {
        let mut r = Reader { b, at: 0 };
        if r.take(4)? != b"PZPA" || r.u32()? != ANALYSIS_VERSION {
            return None;
        }
        let sample_rate = r.f32()?;
        let len = r.u64()? as usize;
        let frames = r.u32()? as usize;
        if frames.saturating_mul(8) > b.len() {
            return None;
        }
        let pitch = (0..frames).map(|_| r.f32()).collect::<Option<Vec<_>>>()?;
        let level = (0..frames).map(|_| r.f32()).collect::<Option<Vec<_>>>()?;
        let count = r.u32()? as usize;
        if count.saturating_mul(28) > b.len() {
            return None;
        }
        let notes = (0..count)
            .map(|_| Some(Note { start: r.f64()?, end: r.f64()?, centre: r.f32()?, vibrato: r.f32()?, level: r.f32()? }))
            .collect::<Option<Vec<_>>>()?;
        (sample_rate > 0.0).then_some(Analysis { sample_rate, len, pitch, level, notes })
    }
}

struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        let s = self.b.get(self.at..self.at.checked_add(n)?)?;
        self.at += n;
        Some(s)
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    fn f32(&mut self) -> Option<f32> {
        Some(f32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn f64(&mut self) -> Option<f64> {
        Some(f64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
}

/// Edits of one note. A note is a span of the file; the list of a file's
/// notes is the analysis' to start with, then cut and joined by the user.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NoteEdit {
    /// Span of the file the note is (seconds).
    pub start: f64,
    pub end: f64,
    /// Where it plays now (seconds of the file's timeline): time edits.
    pub new_start: f64,
    pub new_end: f64,
    /// Centre pitch found in the file, and the one it is moved to (MIDI).
    pub centre: f32,
    pub pitch: f32,
    /// Share of the drift flattened (0: kept, 1: flat).
    pub drift: f32,
    /// Factor on the vibrato (1: as sung, 0: none).
    pub vibrato: f32,
    /// Formant shift, semitones.
    pub formant: f32,
    /// Level change, dB.
    pub gain: f32,
}

impl NoteEdit {
    /// A note as found, unchanged.
    pub fn from_note(n: &Note) -> NoteEdit {
        NoteEdit {
            start: n.start,
            end: n.end,
            new_start: n.start,
            new_end: n.end,
            centre: n.centre,
            pitch: n.centre,
            drift: 0.0,
            vibrato: 1.0,
            formant: 0.0,
            gain: 0.0,
        }
    }

    /// True when the note's sound is changed (pitch, curves, formants, level).
    pub fn sound_edited(&self) -> bool {
        (self.pitch - self.centre).abs() > 1e-4
            || self.drift.abs() > 1e-4
            || (self.vibrato - 1.0).abs() > 1e-4
            || self.formant.abs() > 1e-4
            || self.gain.abs() > 1e-4
    }

    /// True when the note plays at another time or for another length.
    pub fn time_edited(&self) -> bool {
        (self.new_start - self.start).abs() > 1e-6 || (self.new_end - self.end).abs() > 1e-6
    }

    pub fn is_identity(&self) -> bool {
        !self.sound_edited() && !self.time_edited()
    }

    /// Semitones the centre moves by.
    pub fn shift(&self) -> f32 {
        self.pitch - self.centre
    }
}

/// "Correct pitch": the note's centre pulled `amount` (0 … 1) of the way to
/// the nearest note of `mask` (bit 0: C … bit 11: B; 0 means chromatic).
pub fn correct_pitch(e: &mut NoteEdit, mask: u16, amount: f32) {
    let mask = if mask & 0xFFF == 0 { 0xFFF } else { mask };
    if let Some(target) = crate::effects::autotune::nearest(e.pitch, mask) {
        e.pitch += (target as f32 - e.pitch) * amount.clamp(0.0, 1.0);
    }
}

/// True when the edits change nothing: the file plays as is.
pub fn is_identity(edits: &[NoteEdit]) -> bool {
    edits.iter().all(NoteEdit::is_identity)
}

/// Linear interpolation of a frame track at fractional frame `k` (NaN when
/// either side is NaN, the nearest when only one side is past the ends).
pub fn interp(v: &[f32], k: f64) -> f32 {
    if v.is_empty() || !k.is_finite() {
        return f32::NAN;
    }
    let k = k.clamp(0.0, (v.len() - 1) as f64);
    let i = k.floor() as usize;
    let f = (k - i as f64) as f32;
    let a = v[i];
    let b = v.get(i + 1).copied().unwrap_or(a);
    if f <= 0.0 {
        a
    } else {
        a + (b - a) * f
    }
}

/// Centred moving average of the finite values within ±`half` frames (NaN stays NaN).
pub fn smooth_voiced(v: &[f32], half: usize) -> Vec<f32> {
    let n = v.len();
    let mut sum = vec![0.0f64; n + 1];
    let mut cnt = vec![0u32; n + 1];
    for (i, x) in v.iter().enumerate() {
        let ok = x.is_finite();
        sum[i + 1] = sum[i] + if ok { f64::from(*x) } else { 0.0 };
        cnt[i + 1] = cnt[i] + u32::from(ok);
    }
    (0..n)
        .map(|i| {
            if !v[i].is_finite() {
                return f32::NAN;
            }
            let a = i.saturating_sub(half);
            let b = (i + half + 1).min(n);
            let c = cnt[b] - cnt[a];
            if c == 0 { f32::NAN } else { ((sum[b] - sum[a]) / f64::from(c)) as f32 }
        })
        .collect()
}
