//! Quality of the note editing, measured on the Pitch Correction's synthetic
//! singer (true pitch, note boundaries and formants known exactly) with an
//! independent YIN tuner.

use super::*;
use crate::Sample;
use crate::effects::autotune::hz_to_midi;
use crate::effects::autotune::tests::{formants, near};
use crate::effects::autotune::voice::{Style, Sung, VOWELS, envelope, sing, true_pitch};
use ponzik_dsp::tuner::Tuner;
use ponzik_dsp::util::midi_to_hz;

const SR: f32 = 48_000.0;
const SECS: f32 = 3.4;

fn sung(start: f32, dur: f32, midi: f32, vowel: usize) -> Sung {
    Sung { start, dur, midi, vowel }
}

/// A sung line: legato steps (glides), a sibilant, a gap, a leap.
fn melody() -> Vec<Sung> {
    vec![
        sung(0.10, 0.45, 57.0, 0),
        sung(0.55, 0.40, 60.0, 1),
        sung(0.95, 0.50, 62.0, 3),
        sung(1.45, 0.10, f32::NAN, 0),
        sung(1.55, 0.45, 59.0, 2),
        sung(2.00, 0.55, 64.0, 0),
        sung(2.70, 0.45, 62.0, 4),
    ]
}

fn pitched(m: &[Sung]) -> Vec<Sung> {
    m.iter().copied().filter(|n| n.midi.is_finite()).collect()
}

fn sample(x: Vec<f32>) -> Sample {
    Sample { name: "voice".into(), sample_rate: SR, right: x.clone(), left: x, markers: Vec::new() }
}

/// Pitch (time, MIDI) every `hop` samples by an independent tuner.
fn measured(x: &[f32], window: usize, hop: usize) -> Vec<(f32, f32)> {
    let mut t = Tuner::new(SR, window);
    t.set_threshold(0.12);
    let mut out = Vec::new();
    let mut i = 0;
    while i + hop <= x.len() {
        t.push(&x[i..i + hop]);
        i += hop;
        if let Some(p) = t.detect().filter(|p| p.clarity > 0.85) {
            out.push(((i as f32 - window as f32 * 0.5) / SR, hz_to_midi(p.hz, 440.0)));
        }
    }
    out
}

fn median(mut v: Vec<f32>) -> f32 {
    v.sort_by(f32::total_cmp);
    v.get(v.len() / 2).copied().unwrap_or(f32::NAN)
}

/// Median measured pitch over `[a, b)` seconds.
fn pitch_over(track: &[(f32, f32)], a: f32, b: f32) -> f32 {
    median(track.iter().filter(|(t, _)| (a..b).contains(t)).map(|p| p.1).collect())
}

fn analysed(style: &Style) -> (Vec<Sung>, Vec<f32>, Analysis) {
    let m = melody();
    let x = sing(SR, &m, style, SECS);
    let a = analyze(&x, &x, SR);
    (m, x, a)
}

#[test]
fn detects_the_sung_notes() {
    for (name, style) in [
        ("steady", Style { vibrato_cents: 0.0, drift_cents: 0.0, ..Style::default() }),
        ("vibrato + drift", Style::default()),
        ("wide vibrato", Style { vibrato_cents: 55.0, drift_cents: 30.0, ..Style::default() }),
    ] {
        let (m, _, a) = analysed(&style);
        let truth = pitched(&m);
        assert_eq!(a.notes.len(), truth.len(), "{name}: {:?}", a.notes);
        let (mut worst_c, mut worst_s, mut worst_e, mut worst_v) = (0.0f32, 0.0f64, 0.0f64, 0.0f32);
        for (n, t) in a.notes.iter().zip(&truth) {
            // The centre the singer meant: the true pitch averaged over the note held (drift included).
            let steps = 400;
            let (t0, t1) = (t.start + 0.1, t.start + t.dur - 0.05);
            let want = (0..steps).map(|k| true_pitch(&m, &style, t0 + (t1 - t0) * k as f32 / steps as f32)).sum::<f32>() / steps as f32;
            let cents = (n.centre - want) * 100.0;
            let ds = n.start - f64::from(t.start);
            let de = n.end - f64::from(t.start + t.dur);
            worst_c = worst_c.max(cents.abs());
            worst_s = worst_s.max(ds.abs());
            worst_e = worst_e.max(de.abs());
            assert!(cents.abs() < 6.0, "{name}: centre {} for {want} ({cents:.1} cents)", n.centre);
            assert!(ds.abs() < 0.02, "{name}: start {} for {} ({:.1} ms)", n.start, t.start, ds * 1e3);
            assert!(de.abs() < 0.025, "{name}: end {} for {} ({:.1} ms)", n.end, t.start + t.dur, de * 1e3);
            if style.vibrato_cents > 0.0 {
                // The same measure on the true pitch (the synth's vibrato grows in).
                let truth = Analysis { pitch: (0..a.pitch.len()).map(|k| true_pitch(&m, &style, (k as f64 * HOP) as f32)).collect(), ..a.clone() };
                let want = truth.stats(n.start, n.end).unwrap().vibrato;
                worst_v = worst_v.max((n.vibrato - want).abs());
                assert!((n.vibrato - want).abs() < 6.0, "{name}: vibrato {} for {want}", n.vibrato);
            }
        }
        eprintln!(
            "pitch edit, {name}: {} notes of {}, centre within {worst_c:.1} cents, start {:.1} ms, end {:.1} ms, vibrato depth within {worst_v:.1} cents",
            a.notes.len(),
            truth.len(),
            worst_s * 1e3,
            worst_e * 1e3
        );
    }
}

#[test]
fn repeated_notes_on_one_pitch_are_split() {
    // "la-a": the same pitch twice, the level dipping between, no consonant.
    let style = Style { vibrato_cents: 10.0, drift_cents: 0.0, ..Style::default() };
    let m = [sung(0.1, 0.5, 60.0, 0), sung(0.6, 0.5, 60.0, 0)];
    let x = sing(SR, &m, &style, 1.3);
    // The synth fades each note out and in: the dip a singer makes, under 30 ms of silence.
    let a = analyze(&x, &x, SR);
    assert_eq!(a.notes.len(), 2, "{:?}", a.notes);
    assert!((a.notes[1].start - 0.6).abs() < 0.03, "{:?}", a.notes);
}

#[test]
fn analysis_round_trips_and_rejects_garbage() {
    let (_, _, a) = analysed(&Style::default());
    let b = a.to_bytes();
    let back = Analysis::from_bytes(&b).expect("read back");
    assert_eq!(back.to_bytes(), b);
    assert_eq!(back.notes, a.notes);
    assert!(Analysis::from_bytes(&b[..b.len() / 2]).is_none());
    assert!(Analysis::from_bytes(b"PZPA\x63\0\0\0").is_none());
    assert!(Analysis::from_bytes(&[]).is_none());
}

#[test]
fn no_edit_renders_the_file_itself() {
    let (_, x, a) = analysed(&Style::default());
    let s = sample(x);
    let edits: Vec<NoteEdit> = a.notes.iter().map(NoteEdit::from_note).collect();
    let out = render(&s, &a, &edits);
    assert_eq!(out.left, s.left);
}

/// Harmonic amplitudes (dB) of `x` over `[a, b)` s at multiples of `f0` up to `top` Hz.
fn harmonics(x: &[f32], a: f32, b: f32, f0: f32, top: f32) -> Vec<(f32, f32)> {
    let (i0, i1) = ((a * SR) as usize, (b * SR) as usize);
    let n = i1 - i0;
    let mut out = Vec::new();
    let mut k = 1;
    while f0 * k as f32 <= top {
        let f = f0 * k as f32;
        let (mut re, mut im) = (0.0f64, 0.0f64);
        for j in 0..n {
            let w = 0.5 - 0.5 * (std::f64::consts::TAU * j as f64 / n as f64).cos();
            let ph = std::f64::consts::TAU * f64::from(f) * j as f64 / f64::from(SR);
            re += w * f64::from(x[i0 + j]) * ph.cos();
            im -= w * f64::from(x[i0 + j]) * ph.sin();
        }
        out.push((f, (20.0 * (re.hypot(im) + 1e-12).log10()) as f32));
        k += 1;
    }
    out
}

/// RMS distance (dB) between measured harmonics and the vowel's envelope, the best gain fitted.
fn envelope_error(h: &[(f32, f32)], vowel: usize) -> f32 {
    let d: Vec<f32> = h.iter().map(|&(f, db)| db - 20.0 * envelope(f, vowel).log10()).collect();
    let mean = d.iter().sum::<f32>() / d.len() as f32;
    (d.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / d.len() as f32).sqrt()
}

#[test]
fn moving_a_note_hits_the_target_and_keeps_formants() {
    let style = Style { vibrato_cents: 0.0, drift_cents: 0.0, glide: 0.05, ..Style::default() };
    let (m, x, a) = analysed(&style);
    let s = sample(x.clone());
    let mut edits: Vec<NoteEdit> = a.notes.iter().map(NoteEdit::from_note).collect();
    // The third note (62, vowel o) up 3 semitones, the fifth (64, vowel a) down 2.
    edits[2].pitch = edits[2].centre + 3.0;
    edits[4].pitch = edits[4].centre - 2.0;
    let out = render(&s, &a, &edits);
    let tin = measured(&x, 2048, 256);
    let tout = measured(&out.left, 2048, 256);
    for (k, want) in [(2usize, 3.0f32), (4, -2.0)] {
        let n = m.iter().filter(|n| n.midi.is_finite()).nth(k).copied().unwrap();
        let (t0, t1) = (n.start + 0.12, n.start + n.dur - 0.08);
        let moved = (pitch_over(&tout, t0, t1) - pitch_over(&tin, t0, t1)) * 100.0;
        let target = (pitch_over(&tout, t0, t1) - (n.midi + want)) * 100.0;
        eprintln!("pitch edit: note {k} moved {:.1} cents (asked {}), {target:.1} cents off the target", moved, want * 100.0);
        assert!((moved - want * 100.0).abs() < 5.0, "note {k}: {moved} cents");
        assert!(target.abs() < 5.0, "note {k}: {target} cents off");
        // Formants: the harmonics of the moved note follow the vowel's envelope.
        let f0 = midi_to_hz(n.midi + want);
        let err = envelope_error(&harmonics(&out.left, t0, t1, f0, 3_500.0), n.vowel);
        let dry = envelope_error(&harmonics(&x, t0, t1, midi_to_hz(n.midi), 3_500.0), n.vowel);
        // A plain resampling moves the formants with the pitch: how far that is.
        let ratio = 2f32.powf(want / 12.0);
        let naive: Vec<(f32, f32)> = harmonics(&x, t0, t1, midi_to_hz(n.midi), 3_500.0 / ratio).iter().map(|&(f, db)| (f * ratio, db)).collect();
        let naive = envelope_error(&naive, n.vowel);
        // What the harmonics of the file say of the envelope (linear in dB
        // between them): the best any shifter can do at the new harmonics.
        let src = harmonics(&x, t0, t1, midi_to_hz(n.midi), 4_500.0);
        let seen = |f: f32| {
            let i = src.partition_point(|h| h.0 < f).clamp(1, src.len() - 1);
            let (a, b) = (src[i - 1], src[i]);
            a.1 + (b.1 - a.1) * ((f - a.0) / (b.0 - a.0)).clamp(0.0, 1.0)
        };
        let vs_seen = |h: &[(f32, f32)]| {
            let d: Vec<f32> = h.iter().map(|&(f, db)| db - seen(f)).collect();
            let mean = d.iter().sum::<f32>() / d.len() as f32;
            (d.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / d.len() as f32).sqrt()
        };
        let out_h = harmonics(&out.left, t0, t1, f0, 3_500.0);
        let naive_h: Vec<(f32, f32)> = harmonics(&x, t0, t1, midi_to_hz(n.midi), 3_500.0 / ratio).iter().map(|&(f, db)| (f * ratio, db)).collect();
        let (err_seen, naive_seen) = (vs_seen(&out_h), vs_seen(&naive_h));
        eprintln!(
            "pitch edit: note {k} envelope off the vowel's {err:.2} dB (resampled {naive:.2} dB), off the file's harmonics {err_seen:.2} dB (resampled {naive_seen:.2} dB)"
        );
        assert!(dry < 0.1);
        assert!(err_seen < 0.6 * naive_seen, "note {k}: envelope {err_seen} dB, resampled {naive_seen} dB");
        assert!(err < naive, "note {k}: envelope {err} dB, resampled {naive} dB");
        // Level kept.
        let lv = |v: &[f32]| (v[(t0 * SR) as usize..(t1 * SR) as usize].iter().map(|s| s * s).sum::<f32>() / ((t1 - t0) * SR)).sqrt();
        let db = 20.0 * (lv(&out.left) / lv(&x)).log10();
        assert!(db.abs() < 1.0, "note {k}: level {db} dB");
    }
    // The notes not edited are untouched in their middle.
    for k in [0usize, 5] {
        let n = pitched(&m)[k];
        let (i0, i1) = (((n.start + 0.15) * SR) as usize, ((n.start + n.dur - 0.1) * SR) as usize);
        let diff = x[i0..i1].iter().zip(&out.left[i0..i1]).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert!(diff < 1e-6, "note {k} changed by {diff}");
    }
}

#[test]
fn formants_stay_put_or_move_on_purpose() {
    // A low voice (G3, vowel e): LPC sees its formants, not its harmonics.
    let m = [sung(0.05, 1.0, 55.0, 1)];
    let x = sing(SR, &m, &Style { vibrato_cents: 0.0, drift_cents: 0.0, ..Style::default() }, 1.1);
    let a = analyze(&x, &x, SR);
    assert_eq!(a.notes.len(), 1);
    let s = sample(x.clone());
    let at = (0.5 * SR) as usize;
    let f_in = formants(&x, at);
    let (f1, f2) = (near(&f_in, VOWELS[1][0]), near(&f_in, VOWELS[1][1]));
    for (shift, formant) in [(3.0f32, 0.0f32), (-3.0, 0.0), (5.0, 0.0), (0.0, 3.0), (3.0, -3.0)] {
        let mut e = NoteEdit::from_note(&a.notes[0]);
        e.pitch += shift;
        e.formant = formant;
        let out = render(&s, &a, &[e]);
        let f = formants(&out.left, at);
        let expect = 2f32.powf(formant / 12.0);
        let (g1, g2) = (near(&f, f1 * expect), near(&f, f2 * expect));
        let (e1, e2) = ((g1 / (f1 * expect) - 1.0) * 100.0, (g2 / (f2 * expect) - 1.0) * 100.0);
        let p = pitch_over(&measured(&out.left, 2048, 256), 0.3, 0.9);
        eprintln!(
            "pitch edit: {shift:+} st, formants {formant:+} st: F1 {f1:.0} -> {g1:.0} Hz ({e1:+.1} %), F2 {f2:.0} -> {g2:.0} Hz ({e2:+.1} %), pitch {:.1} cents off",
            (p - 55.0 - shift) * 100.0
        );
        assert!(e1.abs() < 6.0 && e2.abs() < 6.0, "{shift} {formant}: F1 {g1} F2 {g2}");
        assert!((p - 55.0 - shift).abs() < 0.05, "{p}");
    }
}

#[test]
fn time_edits_keep_pitch() {
    let style = Style::default();
    let (m, x, a) = analysed(&style);
    let s = sample(x);
    let mut edits: Vec<NoteEdit> = a.notes.iter().map(NoteEdit::from_note).collect();
    // The last note lasts 0.2 s longer (silence after it); the fourth starts 60 ms later.
    let last = edits.len() - 1;
    edits[last].new_end += 0.2;
    edits[3].new_start += 0.06;
    let out = render(&s, &a, &edits);
    let b = analyze(&out.left, &out.right, SR);
    assert_eq!(b.notes.len(), a.notes.len(), "{:?}", b.notes);
    for (k, (n, e)) in b.notes.iter().zip(&edits).enumerate() {
        let dc = (n.centre - e.centre) * 100.0;
        assert!(dc.abs() < 8.0, "note {k}: centre moved {dc} cents");
    }
    let (n, e) = (b.notes[last], edits[last]);
    let (dl, want) = (n.end - n.start, e.new_end - e.new_start);
    eprintln!(
        "pitch edit: stretched note lasts {:.0} ms (asked {:.0}), centre {:.1} cents off; delayed start {:.1} ms off",
        dl * 1e3,
        want * 1e3,
        (n.centre - e.centre) * 100.0,
        (b.notes[3].start - edits[3].new_start) * 1e3
    );
    assert!((dl - want).abs() < 0.03, "length {dl} for {want}");
    assert!((b.notes[3].start - edits[3].new_start).abs() < 0.02, "start {} for {}", b.notes[3].start, edits[3].new_start);
    // Its pitch through the stretch: the sung pitch, same vibrato depth.
    let truth = pitched(&m)[last];
    let tout = measured(&out.left, 2048, 256);
    let p = pitch_over(&tout, (e.new_start + 0.1) as f32, (e.new_end - 0.08) as f32);
    assert!((p - truth.midi).abs() < 0.12, "{p} for {}", truth.midi);
}

/// Largest second difference within `ms` of each time in `at`.
fn worst_d2(x: &[f32], at: &[f64], ms: f64) -> f32 {
    let mut worst = 0.0f32;
    for &t in at {
        let a = ((t - ms / 1e3) * f64::from(SR)).max(2.0) as usize;
        let b = (((t + ms / 1e3) * f64::from(SR)) as usize).min(x.len());
        for i in a..b {
            worst = worst.max((x[i] - 2.0 * x[i - 1] + x[i - 2]).abs());
        }
    }
    worst
}

fn d2_all(x: &[f32]) -> f32 {
    (2..x.len()).map(|i| (x[i] - 2.0 * x[i - 1] + x[i - 2]).abs()).fold(0.0f32, f32::max)
}

#[test]
fn no_clicks_at_note_boundaries() {
    let style = Style::default();
    let (_, x, a) = analysed(&style);
    let s = sample(x.clone());
    let mut edits: Vec<NoteEdit> = a.notes.iter().map(NoteEdit::from_note).collect();
    // Legato neighbours pulled apart, drift flattened, vibrato removed, formants and level moved.
    edits[0].pitch += 2.0;
    edits[1].pitch -= 3.0;
    edits[1].drift = 1.0;
    edits[2].pitch += 4.0;
    edits[2].vibrato = 0.0;
    edits[3].formant = 3.0;
    edits[4].gain = -6.0;
    edits[5].pitch += 1.0;
    edits[5].new_start += 0.03;
    let (out, regions) = render_regions(&s, &a, &edits, None, None, None).unwrap();
    let mut at: Vec<f64> = edits.iter().flat_map(|e| [e.new_start, e.new_end]).collect();
    at.extend(regions.iter().flat_map(|r| [r.0, r.0 + XFADE_T, r.1 - XFADE_T, r.1]));
    let near = worst_d2(&out.left, &at, 8.0);
    // Inside the shifted notes the waveform itself is brighter: compare with them.
    let inside: Vec<f64> = edits.iter().map(|e| 0.5 * (e.new_start + e.new_end)).collect();
    let body = worst_d2(&out.left, &inside, 60.0).max(d2_all(&x));
    eprintln!("pitch edit: second difference at boundaries {near:.4}, in the notes {body:.4} ({:.2}x)", near / body);
    assert!(near < 1.25 * body, "click: {near} against {body}");
}

const XFADE_T: f64 = 0.01;

#[test]
fn unvoiced_parts_are_untouched() {
    let style = Style::default();
    let (m, x, a) = analysed(&style);
    let s = sample(x.clone());
    let mut edits: Vec<NoteEdit> = a.notes.iter().map(NoteEdit::from_note).collect();
    // The notes on both sides of the sibilant move.
    edits[2].pitch += 3.0;
    edits[3].pitch -= 2.0;
    let out = render(&s, &a, &edits);
    let sib = m[3];
    let (i0, i1) = (((sib.start + 0.03) * SR) as usize, ((sib.start + sib.dur - 0.02) * SR) as usize);
    let diff = x[i0..i1].iter().zip(&out.left[i0..i1]).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
    let level = x[i0..i1].iter().fold(0.0f32, |m, v| m.max(v.abs()));
    eprintln!("pitch edit: sibilant between two moved notes changed by {diff:.2e} (peak {level:.3})");
    assert!(diff < 1e-3 * level.max(1e-3), "sibilant changed by {diff}");
}

#[test]
fn rerender_redoes_only_what_changed() {
    let (_, x, a) = analysed(&Style::default());
    let s = sample(x);
    let mut edits: Vec<NoteEdit> = a.notes.iter().map(NoteEdit::from_note).collect();
    edits[0].pitch += 2.0;
    edits[5].pitch -= 1.0;
    let (first, r1) = render_regions(&s, &a, &edits, None, None, None).unwrap();
    assert_eq!(r1.len(), 2);
    let old = edits.clone();
    edits[5].pitch -= 1.0;
    let (second, r2) = render_regions(&s, &a, &edits, Some((&old, &first)), None, None).unwrap();
    assert_eq!(r2.len(), 1, "{r2:?}");
    assert!(r2[0].0 > edits[0].end, "{r2:?}");
    let full = render(&s, &a, &edits);
    let worst = full.left.iter().zip(&second.left).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
    assert!(worst < 1e-6, "incremental render differs by {worst}");
    // Back to no edit: the file again.
    let none: Vec<NoteEdit> = a.notes.iter().map(NoteEdit::from_note).collect();
    let (back, _) = render_regions(&s, &a, &none, Some((&edits, &second)), None, None).unwrap();
    assert_eq!(back.left, s.left);
}

#[test]
fn time_map_follows_the_boundaries() {
    let n = Note { start: 1.0, end: 2.0, centre: 60.0, vibrato: 0.0, level: -10.0 };
    let mut e = NoteEdit::from_note(&n);
    e.new_end = 2.5;
    let map = TimeMap::new(&[e], 4.0);
    assert!((map.src(0.5) - 0.5).abs() < 1e-9);
    assert!((map.src(2.5) - 2.0).abs() < 1e-9);
    assert!((map.src(1.75) - 1.5).abs() < 1e-9);
    assert!((map.dst(1.5) - 1.75).abs() < 1e-9);
    // Between the note's new end and the file's end the gap shrinks.
    assert!((map.src(3.25) - 3.0).abs() < 1e-9);
}

#[test]
fn correct_pitch_pulls_to_the_scale() {
    let n = Note { start: 0.0, end: 1.0, centre: 61.3, vibrato: 0.0, level: -10.0 };
    let mut e = NoteEdit::from_note(&n);
    // C major: C♯ +30 is nearest D (62).
    correct_pitch(&mut e, 0b1010_1011_0101, 0.5);
    assert!((e.pitch - 61.65).abs() < 1e-4, "{}", e.pitch);
    correct_pitch(&mut e, 0b1010_1011_0101, 1.0);
    assert!((e.pitch - 62.0).abs() < 1e-4);
    let mut e = NoteEdit::from_note(&n);
    correct_pitch(&mut e, 0, 1.0);
    assert!((e.pitch - 61.0).abs() < 1e-4);
}

/// The user's own take, analysed and corrected (pitch centres to the nearest
/// semitone, drift halved): `PONZIK_TAKE=<wav>` or the default recording.
#[test]
#[ignore]
fn real_take() {
    let path = std::env::var("PONZIK_TAKE").unwrap_or_else(|_| {
        format!("{}/Musique/Ponzik/Enregistrements/Voix take 1.wav", std::env::var("HOME").unwrap_or_default())
    });
    let s = crate::wav::load_wav(&path).expect("take");
    let t = std::time::Instant::now();
    let a = analyze(&s.left, &s.right, s.sample_rate);
    let secs = t.elapsed().as_secs_f64();
    let voiced = a.pitch.iter().filter(|p| p.is_finite()).count() as f64 * HOP;
    eprintln!(
        "take: {:.1} s at {} Hz, analysed in {secs:.2} s, voiced {voiced:.1} s, {} notes",
        a.duration(),
        s.sample_rate,
        a.notes.len()
    );
    // Median distance (cents) of note centres to their targets; octave slips of the re-analysis left out.
    let off = |d: Vec<f32>| {
        let mut d: Vec<f32> = d.into_iter().map(|c| (c * 100.0).abs()).filter(|c| *c < 600.0).collect();
        d.sort_by(f32::total_cmp);
        d.get(d.len() / 2).copied().unwrap_or(0.0)
    };
    for n in &a.notes {
        eprintln!(
            "  {:6.3}-{:6.3} s  {:5.2} ({:+3.0} c)  vib {:4.0} c  {:5.1} dB",
            n.start,
            n.end,
            n.centre,
            (n.centre - n.centre.round()) * 100.0,
            n.vibrato,
            n.level
        );
    }
    // Frames of the notes held (30 ms off each edge): cents from a target per note.
    let held = |an: &Analysis, target: &dyn Fn(usize) -> f32| -> (f32, f32) {
        let mut d = Vec::new();
        for (i, n) in a.notes.iter().enumerate() {
            let (fa, fb) = an.frames_of(n.start + 0.03, n.end - 0.03);
            d.extend(an.pitch[fa..fb].iter().filter(|p| p.is_finite()).map(|p| ((p - target(i)) * 100.0).abs()));
        }
        d.sort_by(f32::total_cmp);
        let within = d.iter().filter(|c| **c <= 10.0).count() as f32 / d.len().max(1) as f32;
        (d.get(d.len() / 2).copied().unwrap_or(0.0), within * 100.0)
    };
    let nearest = |i: usize| a.notes[i].centre.round();
    let (m0, w0) = held(&a, &nearest);
    eprintln!("take as sung: frames {m0:.1} cents from their note (median), {w0:.0} % within 10 cents");
    let mut edits: Vec<NoteEdit> = a.notes.iter().map(NoteEdit::from_note).collect();
    for drift in [0.5f32, 1.0] {
        for e in &mut edits {
            *e = NoteEdit::from_note(&Note { start: e.start, end: e.end, centre: e.centre, vibrato: 0.0, level: 0.0 });
            correct_pitch(e, 0, 1.0);
            e.drift = drift;
        }
        let t = std::time::Instant::now();
        let out = render(&s, &a, &edits);
        let rsecs = t.elapsed().as_secs_f64();
        crate::wav::write_sample(format!("/tmp/ponzik_take_corrected_drift{}.wav", (drift * 100.0) as u32), &out).expect("write");
        let b = analyze(&out.left, &out.right, out.sample_rate);
        let before: Vec<f32> = a.notes.iter().map(|n| n.centre).collect();
        // The same spans measured again in the render.
        let after: Vec<f32> = a.notes.iter().zip(&edits).filter_map(|(n, e)| Some(b.stats(n.start, n.end)?.centre - e.pitch)).collect();
        let (m1, w1) = held(&b, &nearest);
        if std::env::var_os("PONZIK_TAKE_NOTES").is_some() {
            for (n, e) in a.notes.iter().zip(&edits) {
                let got = b.stats(n.start, n.end).map_or(f32::NAN, |s| s.centre);
                eprintln!("  {:6.3}-{:6.3}  {:6.2} -> {:6.2} got {:6.2} ({:+4.0} c)  vib {:3.0}", n.start, n.end, n.centre, e.pitch, got, (got - e.pitch) * 100.0, n.vibrato);
            }
        }
        let peak = out.left.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        let rms = |x: &[f32]| (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt();
        eprintln!("take: largest second difference {:.4} in the render, {:.4} in the recording", d2_all(&out.left), d2_all(&s.left));
        eprintln!(
            "take, centres 100 %, drift {:.0} %: rendered in {rsecs:.2} s; note centres {:.1} -> {:.1} cents off their target (median); frames {m1:.1} cents (median), {w1:.0} % within 10 cents; {} notes found again; level {:+.2} dB, peak {peak:.3}",
            drift * 100.0,
            off(before.iter().map(|c| c - c.round()).collect()),
            off(after),
            b.notes.len(),
            20.0 * (rms(&out.left) / rms(&s.left)).log10()
        );
        // One note moved again: only its region is redone.
        let old = edits.clone();
        let k = edits.len() / 2;
        edits[k].pitch += 2.0;
        let t = std::time::Instant::now();
        let (_, regions) = render_regions(&s, &a, &edits, Some((&old, &out)), None, None).unwrap();
        eprintln!(
            "take: one note moved again, re-rendered {} region(s), {:.2} s of audio, in {:.3} s",
            regions.len(),
            regions.iter().map(|r| r.1 - r.0).sum::<f64>(),
            t.elapsed().as_secs_f64()
        );
        edits = old;
    }
}

#[test]
fn correct_drift_flattens_the_curve() {
    // Slow drift of ±40 cents, no vibrato: flattened, the pitch holds the centre.
    let style = Style { vibrato_cents: 0.0, drift_cents: 40.0, glide: 0.05, ..Style::default() };
    let m = [sung(0.1, 1.5, 60.3, 0)];
    let x = sing(SR, &m, &style, 1.8);
    let a = analyze(&x, &x, SR);
    assert_eq!(a.notes.len(), 1);
    let s = sample(x.clone());
    let spread = |b: &Analysis| {
        let (fa, fb) = b.frames_of(0.3, 1.45);
        let v: Vec<f32> = b.pitch[fa..fb].iter().copied().filter(|p| p.is_finite()).collect();
        let mean = v.iter().sum::<f32>() / v.len() as f32;
        let dev = v.iter().map(|p| (p - mean).abs()).fold(0.0f32, f32::max) * 100.0;
        (mean, dev)
    };
    let (_, before) = spread(&a);
    for drift in [0.5f32, 1.0] {
        let mut e = NoteEdit::from_note(&a.notes[0]);
        correct_pitch(&mut e, 0, 1.0);
        e.drift = drift;
        let out = render(&s, &a, &[e]);
        let b = analyze(&out.left, &out.right, SR);
        let (mean, after) = spread(&b);
        eprintln!("pitch edit: drift {:.0} %: deviation {before:.1} -> {after:.1} cents, mean {:.1} cents off 60", drift * 100.0, (mean - 60.0) * 100.0);
    }
}
