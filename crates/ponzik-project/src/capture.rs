//! MIDI capture: every note played (controller or computer keyboard) is kept
//! for a few minutes, recording or not; the last phrase can then become a
//! pattern, at the tempo it was played (detected if the transport was stopped).
//!
//! Notes keep their exact timing: nothing is quantized.

use std::collections::VecDeque;

use ponzik_engine::{Note, PPQ};

/// How long played notes are kept, in seconds.
pub const HORIZON: f64 = 300.0;
/// A silence this long (seconds) separates two phrases.
pub const PHRASE_GAP: f64 = 2.5;
/// Onsets closer than this (seconds) are one chord.
const CHORD: f64 = 0.05;
/// Tempo detection range: one octave, the usual reading of a phrase.
const BPM_MIN: f32 = 80.0;
const BPM_MAX: f32 = 160.0;

/// One played note.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LoggedNote {
    pub channel: usize,
    pub key: u8,
    /// In [0, 1].
    pub vel: f32,
    /// Seconds (any monotonic clock).
    pub on: f64,
    /// Release time; `None` while held.
    pub off: Option<f64>,
    /// Playhead in ticks at the note on, if the transport was running.
    pub tick: Option<f64>,
}

impl LoggedNote {
    fn end(&self, now: f64) -> f64 {
        self.off.unwrap_or(now).max(self.on)
    }
}

/// Ring buffer of the last minutes of played notes.
#[derive(Clone, Debug, Default)]
pub struct NoteLog {
    notes: VecDeque<LoggedNote>,
}

impl NoteLog {
    pub fn note_on(&mut self, channel: usize, key: u8, vel: f32, now: f64, tick: Option<f64>) {
        self.prune(now);
        self.notes.push_back(LoggedNote { channel, key, vel, on: now, off: None, tick });
    }

    pub fn note_off(&mut self, channel: usize, key: u8, now: f64) {
        if let Some(n) = self.notes.iter_mut().rev().find(|n| n.channel == channel && n.key == key && n.off.is_none()) {
            n.off = Some(now);
        }
    }

    fn prune(&mut self, now: f64) {
        while self.notes.front().is_some_and(|n| n.end(now) < now - HORIZON) {
            self.notes.pop_front();
        }
    }

    pub fn len(&self) -> usize {
        self.notes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.notes.is_empty()
    }

    /// The last phrase: the latest notes back to a silence of [`PHRASE_GAP`]
    /// (held notes count as sounding). Notes still held end at `now`.
    pub fn last_phrase(&self, now: f64) -> Vec<LoggedNote> {
        let notes: Vec<LoggedNote> = self.notes.iter().filter(|n| n.on >= now - HORIZON).copied().collect();
        let mut start = notes.len();
        // Walking back: the phrase starts after the last silence long enough.
        while start > 0 {
            let i = start - 1;
            if i == 0 {
                start = 0;
                break;
            }
            let sounding_until = notes[..i].iter().map(|n| n.end(now)).fold(f64::MIN, f64::max);
            start = i;
            if notes[i].on - sounding_until >= PHRASE_GAP {
                break;
            }
        }
        notes[start..].iter().map(|n| LoggedNote { off: Some(n.end(now)), ..*n }).collect()
    }
}

/// A captured phrase, ready to become a pattern.
#[derive(Clone, Debug, PartialEq)]
pub struct Captured {
    /// Notes per channel, starting at the phrase's first bar.
    pub notes: Vec<(usize, Note)>,
    /// Pattern length: whole bars.
    pub len: u32,
    pub bpm: f32,
    /// The tempo was detected (the transport was stopped).
    pub detected: bool,
}

/// Turns a phrase into pattern notes. Played along the transport: at the
/// project tempo, in the bar where it started. Otherwise: tempo detected, the
/// first note on the downbeat (silence trimmed).
pub fn capture(phrase: &[LoggedNote], project_bpm: f32, bar: u32) -> Option<Captured> {
    let first = phrase.first()?;
    let bar = bar.max(1) as f64;
    let along = phrase.iter().all(|n| n.tick.is_some());
    let (bpm, origin, detected) = match (along, first.tick) {
        (true, Some(tick)) => (project_bpm, tick - (tick / bar).floor() * bar, false),
        _ => {
            let onsets: Vec<f64> = phrase.iter().map(|n| n.on).collect();
            match detect_tempo(&onsets) {
                Some(bpm) => (bpm, 0.0, true),
                None => (project_bpm, 0.0, false),
            }
        }
    };
    let per_sec = bpm as f64 / 60.0 * PPQ as f64;
    let at = |t: f64| origin + (t - first.on) * per_sec;
    let mut notes = Vec::with_capacity(phrase.len());
    let mut end: f64 = 0.0;
    for n in phrase {
        let start = at(n.on).round().max(0.0);
        let stop = at(n.off.unwrap_or(n.on)).round();
        let len = (stop - start).max(1.0);
        end = end.max(start + len);
        notes.push((n.channel, Note { start: start as u32, len: len as u32, key: n.key, vel: n.vel.clamp(0.0, 1.0) }));
    }
    let len = ((end / bar).ceil().max(1.0) * bar) as u32;
    Some(Captured { notes, len, bpm, detected })
}

/// The tempo a phrase was most likely played at (80 to 160 BPM), from its
/// onsets in seconds; `None` with too few distinct onsets. The first onset is
/// taken as a beat; each candidate is scored on how well onsets fall on its
/// sixteenth grid, preferring onsets on beats and eighths.
pub fn detect_tempo(onsets: &[f64]) -> Option<f32> {
    let mut times: Vec<f64> = onsets.to_vec();
    times.sort_by(f64::total_cmp);
    let mut merged: Vec<f64> = Vec::new();
    for t in times {
        if merged.last().is_none_or(|&l| t - l > CHORD) {
            merged.push(t);
        }
    }
    if merged.len() < 3 {
        return None;
    }
    let t0 = merged[0];
    let cost = |bpm: f32| -> f64 {
        let beat = 60.0 / bpm as f64;
        let mut fit = 0.0;
        let mut level = 0.0;
        for &t in &merged[1..] {
            let x = (t - t0) / beat * 4.0;
            let e = x - x.round();
            fit += e * e * 12.0;
            let step = x.round() as i64;
            level += if e.abs() > 0.2 {
                1.0
            } else if step % 4 == 0 {
                0.0
            } else if step % 2 == 0 {
                0.3
            } else {
                0.6
            };
        }
        let n = (merged.len() - 1) as f64;
        fit / n + 0.15 * level / n
    };
    let mut best = (f64::MAX, BPM_MIN);
    let mut bpm = BPM_MIN;
    while bpm < BPM_MAX {
        let c = cost(bpm);
        if c < best.0 - 1e-9 {
            best = (c, bpm);
        }
        bpm += 0.1;
    }
    Some((best.1 * 10.0).round() / 10.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn played(log: &mut NoteLog, at: f64, len: f64, key: u8, tick: Option<f64>) {
        log.note_on(0, key, 0.8, at, tick);
        log.note_off(0, key, at + len);
    }

    /// Small deterministic jitter, in seconds.
    fn jitter(i: usize) -> f64 {
        ((i * 7919) % 17) as f64 / 17.0 * 0.012 - 0.006
    }

    #[test]
    fn the_last_phrase_starts_after_a_silence() {
        let mut log = NoteLog::default();
        played(&mut log, 0.0, 0.3, 60, None);
        played(&mut log, 0.5, 0.3, 62, None);
        // A long held note bridges a pause: same phrase.
        played(&mut log, 10.0, 3.0, 64, None);
        played(&mut log, 14.0, 0.2, 65, None);
        played(&mut log, 14.5, 0.2, 67, None);
        let p = log.last_phrase(20.0);
        assert_eq!(p.iter().map(|n| n.key).collect::<Vec<_>>(), [64, 65, 67]);
        // A note still held ends now.
        log.note_on(0, 69, 1.0, 15.0, None);
        let p = log.last_phrase(16.0);
        assert_eq!(p.last().map(|n| (n.key, n.off)), Some((69, Some(16.0))));
        // Older than five minutes: forgotten (a held note stays until released).
        log.note_off(0, 69, 16.0);
        log.note_on(0, 70, 1.0, 15.0 + HORIZON + 30.0, None);
        assert_eq!(log.len(), 1);
        assert!(NoteLog::default().last_phrase(0.0).is_empty());
    }

    #[test]
    fn tempo_is_detected_from_loose_playing() {
        for bpm in [92.0f64, 112.0, 128.0, 150.0] {
            let beat = 60.0 / bpm;
            // Eighths with a few sixteenths, a little late or early.
            let steps = [0, 2, 4, 6, 8, 9, 10, 12, 14, 16, 18, 20, 22, 24, 25, 26, 28, 30, 32];
            let onsets: Vec<f64> = steps.iter().enumerate().map(|(i, &s)| 3.0 + s as f64 * beat / 4.0 + jitter(i)).collect();
            let got = detect_tempo(&onsets).unwrap() as f64;
            assert!((got - bpm).abs() <= 1.0, "{bpm}: {got}");
        }
        // Quarters at 90: not read as 135 (eighths off the beat).
        let onsets: Vec<f64> = (0..12).map(|i| i as f64 * 60.0 / 90.0 + jitter(i)).collect();
        assert!((detect_tempo(&onsets).unwrap() - 90.0).abs() <= 1.0);
        // A chord is one onset: too few to tell.
        assert_eq!(detect_tempo(&[0.0, 0.01, 0.02, 1.0]), None);
    }

    #[test]
    fn capture_without_transport_trims_and_keeps_timing() {
        let beat = 60.0 / 120.0;
        let mut log = NoteLog::default();
        // Noise long before, then a phrase starting 7 s in, slightly off-grid.
        played(&mut log, 0.0, 0.1, 40, None);
        let steps = [0, 2, 4, 6, 8, 10, 12, 14, 16, 18];
        for (i, &s) in steps.iter().enumerate() {
            let at = 7.0 + s as f64 * beat / 4.0 + if i == 3 { 0.01 } else { 0.0 };
            played(&mut log, at, beat / 4.0, 60 + i as u8, None);
        }
        let phrase = log.last_phrase(20.0);
        let c = capture(&phrase, 140.0, 384).unwrap();
        assert!(c.detected);
        assert!((c.bpm - 120.0).abs() < 0.5, "{}", c.bpm);
        assert_eq!(c.notes.len(), 10);
        assert_eq!(c.notes[0].1.start, 0);
        // Not quantized: the late note stays late (10 ms ≈ 2 ticks at 120).
        let late = c.notes[3].1.start as f64;
        let grid = 3.0 * 2.0 * 24.0;
        assert!(late > grid && late < grid + 3.5, "{late}");
        assert_eq!(c.notes[1].1.start, 48);
        assert_eq!(c.notes[0].1.len, 24);
        // 18 sixteenths + 1: two bars.
        assert_eq!(c.len, 768);
    }

    #[test]
    fn capture_along_the_transport_uses_the_project_tempo_and_bar() {
        let mut log = NoteLog::default();
        // At 100 BPM, the phrase starts on beat 2 of bar 3 (tick 768 + 96).
        let per_sec = 100.0 / 60.0 * 96.0;
        for i in 0..4 {
            let t = 2.0 + i as f64 * 0.3;
            played(&mut log, t, 0.15, 50, Some(864.0 + (t - 2.0) * per_sec));
        }
        let c = capture(&log.last_phrase(10.0), 100.0, 384).unwrap();
        assert!(!c.detected);
        assert_eq!(c.bpm, 100.0);
        assert_eq!(c.notes[0].1.start, 96);
        assert_eq!(c.notes[1].1.start, 96 + 48);
        assert_eq!(c.notes[0].1.len, 24);
        assert_eq!(c.len, 384);
    }
}
