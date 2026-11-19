//! Audio clip to notes: a recorded monophonic voice or instrument turned into
//! pattern notes, with the analysis of the live voice tracker
//! ([`ponzik_dsp::voice::transcribe`]).

use ponzik_dsp::voice::{VoiceConfig, transcribe};
use ponzik_engine::{Note, PPQ, Sample, TempoMap};

use crate::AudioClipDef;

/// The notes the clip plays, in ticks from its start (only those starting
/// within the clip; the last one is cut at its end). Reversed clips are read
/// as they sound.
pub fn notes_from_audio(clip: &AudioClipDef, sample: &Sample, map: &TempoMap, config: &VoiceConfig) -> Vec<Note> {
    let sr = sample.sample_rate.max(1.0) as f64;
    let mut mono = ponzik_dsp::onset::mono(&sample.left, &sample.right);
    if clip.reverse {
        mono.reverse();
    }
    let warp = clip.warp_map();
    let clip_secs = map.secs_at(clip.start as f64);
    // Tick from the clip start of a position in the file.
    let tick_of = |pos: usize| {
        let secs = pos as f64 / sr;
        let tick = if clip.warp {
            clip.start as f64 + (warp.beat_at(secs) - warp.beat_at(clip.offset)) * PPQ as f64
        } else {
            map.tick_at_secs(clip_secs + secs - clip.offset)
        };
        tick - clip.start as f64
    };
    let len = clip.len as f64;
    transcribe(&mono, sample.sample_rate, config)
        .into_iter()
        .filter_map(|n| {
            let (a, b) = (tick_of(n.start), tick_of(n.end).min(len));
            if a < -0.5 || a >= len || b <= a {
                return None;
            }
            let start = a.round().max(0.0) as u32;
            let end = (b.round() as u32).max(start + 1);
            Some(Note { start, len: end - start, key: n.key, vel: n.velocity, ..Note::DEFAULT })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    /// A sung line at 48 kHz: (MIDI key, seconds) with short gaps.
    fn sung(notes: &[(f64, f64)], gap: f64) -> Vec<f32> {
        let sr = 48_000.0;
        let mut out = vec![0.0; (gap * sr) as usize];
        for &(key, secs) in notes {
            let mut phase = 0.0;
            for i in 0..(secs * sr) as usize {
                let t = i as f64 / sr;
                let midi = key + 0.25 * (TAU * 5.0 * t).sin();
                phase += 440.0 * 2f64.powf((midi - 69.0) / 12.0) / sr;
                let x: f64 = (1..=10).map(|h| (TAU * phase * h as f64).sin() / h as f64).sum();
                let env = (t / 0.015).min(1.0) * ((secs - t) / 0.02).clamp(0.0, 1.0);
                out.push((0.15 * x * env) as f32);
            }
            out.extend(std::iter::repeat_n(0.0, (gap * sr) as usize));
        }
        out
    }

    #[test]
    fn a_sung_clip_becomes_notes_on_the_beat() {
        // 120 BPM: a beat lasts 0.5 s. Four notes of 0.4 s, one per beat.
        let x = sung(&[(57.0, 0.4), (60.0, 0.4), (64.0, 0.4), (62.0, 0.4)], 0.1);
        let sample = Sample { name: "voice".into(), sample_rate: 48_000.0, right: x.clone(), left: x, markers: Vec::new() };
        let map = TempoMap::constant(120.0);
        let ppq = PPQ;
        let clip = AudioClipDef { start: 4 * ppq, len: 4 * ppq, offset: 0.1, ..Default::default() };
        let notes = notes_from_audio(&clip, &sample, &map, &VoiceConfig::default());
        assert_eq!(notes.iter().map(|n| n.key).collect::<Vec<_>>(), [57, 60, 64, 62]);
        for (i, n) in notes.iter().enumerate() {
            let beat = i as f64 * ppq as f64;
            assert!((n.start as f64 - beat).abs() < ppq as f64 * 0.06, "note {i} at {} ticks", n.start);
            assert!((n.len as f64 - 0.8 * ppq as f64).abs() < ppq as f64 * 0.15, "note {i} lasts {} ticks", n.len);
        }
        // A shorter clip keeps only what it plays.
        let short = AudioClipDef { len: 2 * ppq, ..clip };
        assert_eq!(notes_from_audio(&short, &sample, &map, &VoiceConfig::default()).len(), 2);
    }

    #[test]
    fn the_project_scale_as_a_mask() {
        let mut p = crate::demo_project();
        p.key = 9;
        p.scale = crate::ScaleKind::Minor;
        // A minor: A B C D E F G.
        let want: u16 = [9, 11, 0, 2, 4, 5, 7].iter().map(|&c| 1 << c).sum();
        assert_eq!(p.scale_mask(), want);
        p.scale = crate::ScaleKind::Chromatic;
        assert_eq!(p.scale_mask(), 0xFFF);
    }
}
