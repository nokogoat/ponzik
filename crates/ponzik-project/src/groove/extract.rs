//! Grooves taken from played material: notes (a pattern, a MIDI file, a
//! recorded take), the attacks of an audio clip on the timeline, or of a
//! loose loop whose tempo is guessed from its length and its beat.

use ponzik_dsp::onset::{OnsetAnalysis, mono};
use ponzik_engine::{Note, PPQ, Sample, TICKS_PER_BAR, WarpMarker};

use super::{Groove, MAX_GROOVE_BARS};
use crate::{AudioClipDef, TempoMap};

/// Attacks closer than this to a stronger one (in ticks) are the same hit
/// (a flam, two drums together).
const SAME_HIT: f64 = 3.0;

/// Thirty-seconds when the hits split sixteenths: at least three sixteenths
/// holding two hits a thirty-second apart or so.
fn steps_of(hits: &[(f64, f32)]) -> u32 {
    let mut cells: Vec<(i64, f64, f64)> = Vec::new();
    for &(t, _) in hits {
        let q = super::grid_point(t.round() as i64, 24);
        match cells.iter_mut().find(|c| c.0 == q) {
            Some(c) => {
                c.1 = c.1.min(t);
                c.2 = c.2.max(t);
            }
            None => cells.push((q, t, t)),
        }
    }
    let split = cells.iter().filter(|c| c.2 - c.1 >= 9.0).count();
    if split >= 3 { 32 } else { 16 }
}

/// Groove of hits at (tick, strength), the groove repeating every `bars`
/// bars (1 to 4): the average offset and relative strength of the hits of
/// each step. A step without a hit takes the same step of the other bars,
/// else stays straight. `None` without any hit.
pub fn from_hits(name: impl Into<String>, hits: &[(f64, f32)], bars: u32) -> Option<Groove> {
    if hits.is_empty() {
        return None;
    }
    let steps = steps_of(hits);
    let bars = bars.clamp(1, MAX_GROOVE_BARS);
    let mut g = Groove::straight(name, steps, bars);
    let n = g.timing.len();
    let st = g.step_ticks() as i64;
    let (mut dev, mut vel, mut count) = (vec![0.0f64; n], vec![0.0f64; n], vec![0u32; n]);
    let mut total = 0.0f64;
    for &(t, v) in hits {
        let q = super::grid_point(t.round() as i64, st);
        let k = (q / st).rem_euclid(n as i64) as usize;
        dev[k] += t - q as f64;
        vel[k] += v.max(0.0) as f64;
        count[k] += 1;
        total += v.max(0.0) as f64;
    }
    let mean = total / hits.len() as f64;
    let s = steps as usize;
    for k in 0..n {
        // Missing step: the same step of the other bars.
        let (d, v, c) = if count[k] > 0 {
            (dev[k], vel[k], count[k])
        } else {
            (0..bars as usize).map(|b| b * s + k % s).fold((0.0, 0.0, 0), |a, j| (a.0 + dev[j], a.1 + vel[j], a.2 + count[j]))
        };
        if c > 0 {
            g.timing[k] = (d / c as f64) as f32;
            if mean > 0.0 {
                g.velocity[k] = (v / c as f64 / mean) as f32;
            }
        }
    }
    g.normalize();
    Some(g)
}

/// Groove of notes (every channel together, slides aside) over `bars` bars.
pub fn from_notes<'a>(name: impl Into<String>, notes: impl IntoIterator<Item = &'a Note>, bars: u32) -> Option<Groove> {
    let hits: Vec<(f64, f32)> = notes.into_iter().filter(|n| !n.slide).map(|n| (n.start as f64, n.vel)).collect();
    from_hits(name, &hits, bars)
}

/// Groove of a MIDI file's notes (every track), over its first bars.
pub fn groove_from_midi(bytes: &[u8], name: impl Into<String>) -> Result<Groove, String> {
    let file = crate::midi_file::read_smf(bytes)?;
    let notes: Vec<Note> = file.tracks.iter().flat_map(|t| t.notes.iter().copied()).collect();
    let end = notes.iter().map(|n| n.start).max().unwrap_or(0);
    let bars = (end / TICKS_PER_BAR + 1).min(MAX_GROOVE_BARS);
    from_notes(name, &notes, bars).ok_or_else(|| ponzik_i18n::tr("No notes to take a groove from").to_string())
}

/// Attacks of a sample: (position in seconds, peak level just after it).
fn attacks(sample: &Sample) -> Vec<(f64, f32)> {
    let sr = sample.sample_rate.max(1.0) as f64;
    let m = mono(&sample.left, &sample.right);
    let analysis = OnsetAnalysis::new(&m, sample.sample_rate);
    let peak_span = (0.01 * sr) as usize;
    let mut out: Vec<(f64, f32)> = analysis
        .pick(&m, 0.6, 4_096)
        .iter()
        .map(|o| (o.pos as f64 / sr, m[o.pos..(o.pos + peak_span).min(m.len())].iter().fold(0.0f32, |a, x| a.max(x.abs()))))
        .collect();
    // The strongest of hits that land together.
    out.dedup_by(|b, a| {
        let same = b.0 - a.0 < 0.012;
        if same && b.1 > a.1 {
            a.1 = b.1;
        }
        same
    });
    out
}

/// Merges hits closer than [`SAME_HIT`] ticks, keeping the first position
/// and the loudest level.
fn merge(mut hits: Vec<(f64, f32)>) -> Vec<(f64, f32)> {
    hits.sort_by(|a, b| a.0.total_cmp(&b.0));
    hits.dedup_by(|b, a| {
        let same = b.0 - a.0 < SAME_HIT;
        if same {
            a.1 = a.1.max(b.1);
        }
        same
    });
    hits
}

/// Groove of the attacks of an audio clip playing `sample`: each attack
/// placed on the song's timeline (the clip's warp or the tempo map), its
/// peak level as accent. `None` without attacks.
pub fn groove_from_audio(clip: &AudioClipDef, sample: &Sample, map: &TempoMap, name: impl Into<String>) -> Option<Groove> {
    let warp = clip.warp_map();
    let clip_secs = map.secs_at(clip.start as f64);
    let hits: Vec<(f64, f32)> = attacks(sample)
        .into_iter()
        .filter_map(|(secs, peak)| {
            let tick = if clip.warp {
                clip.start as f64 + (warp.beat_at(secs) - warp.beat_at(clip.offset)) * PPQ as f64
            } else {
                map.tick_at_secs(clip_secs + secs - clip.offset)
            };
            let local = tick - clip.start as f64;
            (local >= -0.5 && local < clip.len as f64).then_some((local.max(0.0), peak))
        })
        .collect();
    let bars = clip.len.div_ceil(TICKS_PER_BAR);
    from_hits(name, &merge(hits), bars)
}

/// Tempo of a loop that lasts a whole number of bars: the number of bars
/// (1, 2, 4 or 8) whose tempo is closest to the detected beat (120 BPM
/// without a clear beat), between 60 and 200 BPM.
pub fn loop_tempo(sample: &Sample) -> Option<(f32, u32)> {
    let secs = sample.left.len() as f32 / sample.sample_rate.max(1.0);
    if secs < 0.25 {
        return None;
    }
    let m = mono(&sample.left, &sample.right);
    let analysis = OnsetAnalysis::new(&m, sample.sample_rate);
    let guess = ponzik_dsp::tempo::estimate_bpm(&analysis, secs).unwrap_or(120.0);
    [1u32, 2, 4, 8]
        .into_iter()
        .map(|b| (b as f32 * 240.0 / secs, b))
        .filter(|(bpm, _)| (60.0..=200.0).contains(bpm))
        .min_by(|a, b| (a.0 / guess).ln().abs().total_cmp(&(b.0 / guess).ln().abs()))
}

/// Groove of a loop that is not on the timeline (a sample in the browser or
/// the Sample Lab): its tempo from its length and beat, the loop starting on
/// a downbeat. Returns the groove and the tempo it was read at.
pub fn groove_from_loop(sample: &Sample, name: impl Into<String>) -> Option<(Groove, f32)> {
    let (bpm, bars) = loop_tempo(sample)?;
    let ticks_per_sec = bpm as f64 / 60.0 * PPQ as f64;
    let len = (bars * TICKS_PER_BAR) as f64;
    let hits: Vec<(f64, f32)> = attacks(sample).into_iter().map(|(s, p)| (s * ticks_per_sec, p)).filter(|h| h.0 < len - 0.5).collect();
    Some((from_hits(name, &merge(hits), bars)?, bpm))
}

/// Warp markers that make a warped clip play `groove`: each attack of the
/// clip moved to where the groove plays a note started there (song ticks, so
/// the groove lines up with the bars). `None` if the clip is not warped or
/// has no attacks.
pub fn groove_warp_markers(clip: &AudioClipDef, sample: &Sample, groove: &Groove) -> Option<Vec<WarpMarker>> {
    if !clip.warp {
        return None;
    }
    let warp = clip.warp_map();
    let base = warp.beat_at(clip.offset);
    let mut markers: Vec<WarpMarker> = Vec::new();
    for (secs, _) in attacks(sample) {
        let beat = warp.beat_at(secs);
        let tick = clip.start as f64 + (beat - base) * PPQ as f64;
        if tick < clip.start as f64 || tick >= (clip.start + clip.len) as f64 {
            continue;
        }
        let note = Note::new(tick.round() as u32, 1, 60, 1.0);
        let moved = groove.apply(&note, 0).start as f64 + (tick - tick.round());
        let new_beat = base + (moved - clip.start as f64) / PPQ as f64;
        if markers.last().is_none_or(|m| secs > m.secs + 1e-3 && new_beat > m.beat + 1e-3) {
            markers.push(WarpMarker { secs, beat: new_beat });
        }
    }
    (!markers.is_empty()).then_some(markers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TempoMap;

    /// Noise hits of `level` at each (seconds, level), at 48 kHz, decaying
    /// over a few milliseconds like a hi-hat, over a faint noise floor.
    fn clicks(secs: f64, at: &[(f64, f32)]) -> Sample {
        let sr = 48_000.0;
        let noise = |i: usize| (ponzik_engine::splitmix(i as u64) >> 40) as f32 / (1u64 << 23) as f32 - 1.0;
        let mut left: Vec<f32> = (0..(secs * sr) as usize).map(|i| 0.002 * noise(i + 7_777_777)).collect();
        for &(s, level) in at {
            let i0 = (s * sr).round() as usize;
            for i in 0..2_400.min(left.len().saturating_sub(i0)) {
                left[i0 + i] += level * (-(i as f32) / 300.0).exp() * noise(i0 * 31 + i);
            }
        }
        Sample { name: "loop".into(), sample_rate: sr as f32, right: left.clone(), left, markers: Vec::new() }
    }

    #[test]
    fn swing_of_a_loose_loop_is_recovered_within_a_few_ms() {
        // Two bars at 96 BPM, hats on every sixteenth swung at 62 %, the
        // beats louder: the loop is not on the timeline.
        let bpm = 96.0f64;
        let six = 60.0 / bpm / 4.0;
        let late = (0.62 / 0.5 - 1.0) * six;
        let hits: Vec<(f64, f32)> =
            (0..32).map(|s| (s as f64 * six + if s % 2 == 1 { late } else { 0.0 }, if s % 4 == 0 { 0.9 } else { 0.4 })).collect();
        let sample = clicks(8.0 * 60.0 / bpm, &hits);
        let (g, found) = groove_from_loop(&sample, "loop").unwrap();
        assert!((found as f64 - bpm).abs() < 0.01, "{found}");
        let ms_per_tick = 60_000.0 / bpm / PPQ as f64;
        let want = late * 1000.0;
        let mut worst = 0.0f64;
        for s in 0..g.timing.len() {
            let got = g.timing[s] as f64 * ms_per_tick;
            let err = (got - if s % 2 == 1 { want } else { 0.0 }).abs();
            worst = worst.max(err);
        }
        eprintln!("loop swing: {:.1} ms late wanted, worst error {:.2} ms", want, worst);
        assert!(worst < 3.0, "{worst} ms: {:?}", g.timing);
        assert!(g.velocity[0] > 1.4 && g.velocity[1] < 0.8, "{:?}", g.velocity);
    }

    #[test]
    fn groove_of_a_recorded_beat() {
        // 120 BPM (250 samples a tick at 48 kHz): clicks on every sixteenth,
        // the off ones 8 ticks late, the beats louder; the clip starts on bar 2.
        let hits: Vec<(f64, f32)> = (0..16)
            .map(|s| ((s * 24 + if s % 2 == 1 { 8 } else { 0 }) as f64 * 250.0 / 48_000.0, if s % 4 == 0 { 0.9 } else { 0.45 }))
            .collect();
        let sample = clicks(2.0, &hits);
        let clip = AudioClipDef { start: 384, len: 384, ..Default::default() };
        let map = TempoMap::new(vec![ponzik_engine::TempoPoint { tick: 0, bpm: 120.0, ramp: false }], Vec::new());
        let g = groove_from_audio(&clip, &sample, &map, "beat").unwrap();
        for s in 0..16 {
            let want = if s % 2 == 1 { 8.0 } else { 0.0 };
            assert!((g.timing[s] - want).abs() <= 1.0, "{s}: {:?}", g.timing);
        }
        assert!(g.velocity[0] > 1.3 && g.velocity[1] < 0.9, "{:?}", g.velocity);
    }

    #[test]
    fn notes_with_thirty_seconds_give_a_finer_groove() {
        let mut notes: Vec<Note> = (0..16).map(|s| Note::new(s * 24, 6, 42, 0.6)).collect();
        notes.extend((0..4).map(|b| Note::new(b * 96 + 12 + 1, 6, 42, 0.4)));
        let g = from_notes("rolls", &notes, 1).unwrap();
        assert_eq!(g.steps, 32);
        assert_eq!(g.timing[1], 1.0);
        let straight: Vec<Note> = (0..16).map(|s| Note::new(s * 24 + (s % 2) * 9, 6, 42, 0.6)).collect();
        assert_eq!(from_notes("swing", &straight, 1).unwrap().steps, 16);
        assert!(from_notes("none", &[], 1).is_none());
    }

    #[test]
    fn two_bar_grooves_fill_missing_steps_from_the_other_bar() {
        // Bar 1 swung by 6, bar 2 only plays its downbeat.
        let mut notes: Vec<Note> = (0..16).map(|s| Note::new(s * 24 + (s % 2) * 6, 6, 42, 0.6)).collect();
        notes.push(Note::new(384 + 2, 6, 36, 0.6));
        let g = from_notes("two", &notes, 2).unwrap();
        assert_eq!(g.bars(), 2);
        assert_eq!(g.timing[16], 2.0);
        assert_eq!(g.timing[16 + 3], 6.0);
    }

    #[test]
    fn midi_files_give_grooves() {
        let mut p = crate::Project::empty();
        p.add_channel("A", crate::InstrumentDef::new("pulse"), 60);
        for s in 0..16 {
            p.add_note(0, 0, Note::new(s * 24 + if s % 2 == 1 { 10 } else { 0 }, 12, 42, 0.8));
        }
        let bytes = crate::midi_file::write_smf(&p, 0);
        let g = groove_from_midi(&bytes, "mid").unwrap();
        assert_eq!(g.timing[1], 10.0);
        assert!(groove_from_midi(b"junk", "x").is_err());
    }

    #[test]
    fn warp_markers_play_the_groove() {
        // Straight sixteenths at 120 BPM, the clip warped at its own tempo.
        let hits: Vec<(f64, f32)> = (0..16).map(|s| (s as f64 * 0.125, 0.8)).collect();
        let sample = clicks(2.0, &hits);
        let clip = AudioClipDef { start: 0, len: 384, warp: true, bpm: 120.0, ..Default::default() };
        let markers = groove_warp_markers(&clip, &sample, &Groove::swing(75.0)).unwrap();
        assert!(markers.len() >= 14, "{}", markers.len());
        for m in &markers {
            let step = (m.secs / 0.125).round() as i64;
            let want = step as f64 * 0.25 + if step % 2 == 1 { 0.125 } else { 0.0 };
            assert!((m.beat - want).abs() < 0.02, "{step}: {} vs {want}", m.beat);
        }
        assert!(groove_warp_markers(&AudioClipDef { warp: false, ..clip.clone() }, &sample, &Groove::swing(66.0)).is_none());
    }
}
