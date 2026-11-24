//! Quality of the pitch correction, measured on a synthetic singer.

use super::voice::{Style, Sung, sing, true_pitch};
use super::*;
use crate::effects::testing::{SR, assert_stable_at_extremes, noise, rms, run};
use ponzik_dsp::tuner::Tuner;

fn note(start: f32, dur: f32, midi: f32) -> Sung {
    Sung { start, dur, midi, vowel: 0 }
}

fn tune(params: &[(u32, f32)]) -> AutoTune {
    let mut a = AutoTune::default();
    for &(id, v) in params {
        a.set_param(id, v);
    }
    a.reset();
    a
}

fn process(a: &mut AutoTune, x: &[f32]) -> Vec<f32> {
    let (mut l, mut r) = (x.to_vec(), x.to_vec());
    run(a, &mut l, &mut r);
    l
}

/// Pitch of `x` (MIDI) every `hop` samples by an independent YIN tuner, with the window center times.
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

fn percentile(mut v: Vec<f32>, p: f32) -> f32 {
    v.sort_by(f32::total_cmp);
    v.get(((v.len() as f32 - 1.0) * p).round() as usize).copied().unwrap_or(f32::NAN)
}

/// C major, the scale of most tests.
const C_MAJOR: [(u32, f32); 3] = [(P_KEY, 0.0), (P_SCALE, 0.0), (P_FOLLOW, 0.0)];

#[test]
fn detection_is_accurate_across_ranges() {
    let style = Style { vibrato_cents: 35.0, drift_cents: 25.0, ..Style::default() };
    let mut report = Vec::new();
    for (range, &(low, high)) in RANGE_NOTES.iter().enumerate() {
        for quality in [false, true] {
            let mut det = Detector::new(SR);
            det.configure(range, 0.5, quality);
            let (mut errs, mut octave, mut voiced, mut frames) = (Vec::new(), 0, 0, 0);
            for m in [low as f32 + 1.0, (low as f32 + high as f32) * 0.5, high as f32 - 2.0] {
                for vowel in [0, 2, 4] {
                    let notes = [Sung { start: 0.05, dur: 0.8, midi: m, vowel }];
                    let x = sing(SR, &notes, &style, 0.9);
                    det.reset();
                    let (v0, f0) = (voiced, frames);
                    for (i, &s) in x.iter().enumerate() {
                        if let Some(f) = det.push(s) {
                            let t = f.time as f32 / SR;
                            let truth = true_pitch(&notes, &style, t);
                            if !(0.15..0.8).contains(&t) || !truth.is_finite() || i < 4_800 {
                                continue;
                            }
                            frames += 1;
                            if f.voiced {
                                voiced += 1;
                                let e = (hz_to_midi(f.hz, 440.0) - truth) * 100.0;
                                if e.abs() > 600.0 {
                                    octave += 1;
                                } else {
                                    errs.push(e.abs());
                                }
                            }
                        }
                    }
                    if std::env::var_os("AT_DEBUG").is_some() {
                        eprintln!("{} m {m} vowel {vowel}: {}/{}", RANGES[range], voiced - v0, frames - f0);
                    }
                }
            }
            let (mean, p95) = (errs.iter().sum::<f32>() / errs.len() as f32, percentile(errs.clone(), 0.95));
            let voiced_pct = 100.0 * voiced as f32 / frames as f32;
            report.push(format!("{:>10} {:>7}: mean {mean:.2} ct, p95 {p95:.2} ct, octave errors {octave}/{frames}, voiced {voiced_pct:.1}%", RANGES[range], if quality { "quality" } else { "low" }));
            assert!(mean < 4.0 && p95 < 10.0, "{}", report.last().unwrap());
            assert_eq!(octave, 0, "{}", report.last().unwrap());
            assert!(voiced_pct > 97.0, "{}", report.last().unwrap());
        }
    }
    eprintln!("{}", report.join("\n"));
}

/// A melody sung off pitch: notes of C major, each sung up to 40 cents off,
/// with vibrato and drift.
fn melody(base: f32) -> (Vec<Sung>, Vec<f32>) {
    let steps = [0.0, 2.0, 4.0, 5.0, 7.0, 4.0, 2.0, 0.0];
    let offs = [0.3, -0.25, 0.28, -0.22, 0.2, -0.28, 0.25, -0.18];
    let notes: Vec<Sung> = (0..8).map(|i| Sung { start: 0.1 + 0.45 * i as f32, dur: 0.45, midi: base + steps[i] + offs[i], vowel: i % 5 }).collect();
    let targets = (0..8).map(|i| base + steps[i]).collect();
    (notes, targets)
}

/// Deviation (cents) of the output from the note each sung note should land
/// on, over the steady middle of the notes: (median, p95, worst).
fn landing(y: &[f32], notes: &[Sung], targets: &[f32], latency: usize) -> (f32, f32, f32) {
    let lat = latency as f32 / SR;
    let mut devs = Vec::new();
    for (t, m) in measured(y, 1_024, 256) {
        let t = t - lat;
        if let Some(i) = notes.iter().position(|n| t > n.start + 0.12 && t < n.start + n.dur - 0.05) {
            devs.push(((m - targets[i]) * 100.0).abs());
            if latency > 0 && std::env::var_os("AT_DEBUG").is_some() && devs.last().is_some_and(|d| *d > 50.0) {
                eprintln!("note {i} at {t:.3}: {m:.2} vs {}", targets[i]);
            }
        }
    }
    assert!(devs.len() > 100, "{} points", devs.len());
    let worst = devs.iter().copied().fold(0.0, f32::max);
    (percentile(devs.clone(), 0.5), percentile(devs, 0.95), worst)
}

#[test]
fn hard_tune_lands_on_the_scale() {
    let style = Style { vibrato_cents: 15.0, drift_cents: 12.0, ..Style::default() };
    let mut report = Vec::new();
    for (mode, latency) in [(0.0, 1.0), (0.0, 0.0), (1.0, 1.0), (1.0, 0.0)] {
        for (range, base) in [(0, 67.0), (1, 60.0), (2, 48.0), (3, 43.0)] {
            let (notes, targets) = melody(base);
            let x = sing(SR, &notes, &style, 3.9);
            let mut a = tune(&[C_MAJOR[0], C_MAJOR[1], C_MAJOR[2], (P_RETUNE, 0.0), (P_MODE, mode), (P_LATENCY, latency), (P_RANGE, range as f32)]);
            let y = process(&mut a, &x);
            let dry = landing(&x, &notes, &targets, 0);
            let (med, p95, worst) = landing(&y, &notes, &targets, a.latency());
            report.push(format!(
                "{} {} {:>8}: dry median {:.1} ct; tuned median {med:.2} ct, p95 {p95:.2} ct, worst {worst:.1} ct, latency {:.2} ms",
                MODES[mode as usize], LATENCIES[latency as usize], RANGES[range], dry.0, a.latency() as f32 * 1000.0 / SR
            ));
            assert!(med < 3.0 && p95 < 8.0 && worst < 25.0, "{}", report.last().unwrap());
        }
    }
    eprintln!("{}", report.join("\n"));
}

/// Writes WAV files to `/tmp` to listen to: a synthetic hook in A minor sung
/// off pitch, dry and through the quick modes; the user's own vocal take
/// when there is one (`PONZIK_VOCAL=<file.wav>`, or the recordings folder).
/// `cargo test --release -p ponzik-engine --lib autotune::tests::demo -- --ignored --nocapture`
#[test]
#[ignore]
fn demo() {
    // A minor hook: A C D E D C A G, sung 20 to 35 cents off, with sibilants.
    let steps = [57.0, 60.0, 62.0, 64.0, 62.0, 60.0, 57.0, 55.0];
    let offs = [0.3, -0.25, 0.22, -0.32, 0.28, -0.2, 0.33, -0.24];
    let mut notes = Vec::new();
    for i in 0..8 {
        let start = 0.15 + 0.5 * i as f32;
        if i % 3 == 2 {
            notes.push(Sung { start, dur: 0.08, midi: f32::NAN, vowel: 0 });
        }
        let lead = if i % 3 == 2 { 0.08 } else { 0.0 };
        notes.push(Sung { start: start + lead, dur: 0.5 - lead, midi: steps[i] + offs[i], vowel: (i * 2) % 5 });
    }
    let style = Style { vibrato_cents: 28.0, drift_cents: 18.0, ..Style::default() };
    let x = sing(SR, &notes, &style, 4.4);
    let a_minor = [(P_KEY, 9.0), (P_SCALE, 1.0), (P_FOLLOW, 0.0)];
    let write = |name: &str, y: &[f32]| {
        let stereo: Vec<f32> = y.iter().flat_map(|&s| [s, s]).collect();
        crate::wav::write_wav_i16(format!("/tmp/autotune_{name}.wav"), SR as u32, 2, &stereo).unwrap();
    };
    write("dry", &x);
    let targets: Vec<f32> = notes.iter().map(|n| if n.midi.is_finite() { n.midi.round() } else { f32::NAN }).collect();
    let dev = |y: &[f32], lat: usize| landing(y, &notes, &targets, lat);
    eprintln!("dry: median {:.1} ct off the notes", dev(&x, 0).0);
    for (i, (name, values)) in QUICK_MODES.iter().enumerate() {
        let mut params: Vec<(u32, f32)> = a_minor.to_vec();
        params.extend_from_slice(values);
        let mut a = tune(&params);
        let (mut l, mut r) = (x.clone(), x.clone());
        let t0 = std::time::Instant::now();
        run(&mut a, &mut l, &mut r);
        let cpu = t0.elapsed().as_secs_f32() / (x.len() as f32 / SR) * 100.0;
        eprintln!("  cpu {cpu:.2} % of one core, clicks: second difference {:.2} x the dry", spikes(&l) / spikes(&x));
        let file = ["default", "hard", "natural", "pop", "rap", "choir"][i];
        let stereo: Vec<f32> = l.iter().zip(&r).flat_map(|(&a, &b)| [a, b]).collect();
        crate::wav::write_wav_i16(format!("/tmp/autotune_{file}.wav"), SR as u32, 2, &stereo).unwrap();
        let (med, p95, _) = dev(&l, a.latency());
        eprintln!("{name}: median {med:.1} ct, p95 {p95:.1} ct off the notes, latency {:.1} ms -> /tmp/autotune_{file}.wav", a.latency() as f32 * 1000.0 / SR);
    }
    let mut a = tune(&[a_minor[0], a_minor[1], a_minor[2], (P_RETUNE, 0.0), (P_TRANSPOSE, -5.0), (P_FORMANT_SHIFT, -3.0)]);
    let y = process(&mut a, &x);
    write("deep", &y);
    // A real take, if there is one.
    let home = std::env::var("HOME").unwrap_or_default();
    let path = std::env::var("PONZIK_VOCAL").unwrap_or(format!("{home}/Musique/Ponzik/Enregistrements/Voix take 1.wav"));
    if let Ok(take) = crate::wav::load_wav(&path) {
        let sr = take.sample_rate;
        eprintln!("vocal take {path}: {:.1} s at {sr} Hz", take.duration());
        for (file, params) in [("take_hard", vec![(P_RETUNE, 0.0), (P_RANGE, 3.0)]), ("take_natural", vec![(P_RETUNE, 0.08), (P_HUMANIZE, 0.4), (P_RANGE, 3.0)])] {
            let mut a = AutoTune::default();
            a.set_sample_rate(sr);
            for (id, v) in params {
                a.set_param(id, v);
            }
            let (mut l, mut r) = (take.left.clone(), take.right.clone());
            for (cl, cr) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
                a.process(cl, cr);
            }
            let voiced = {
                let mut d = Detector::new(sr);
                d.configure(3, 0.5, true);
                let (mut v, mut n) = (0, 0);
                for &s in &take.left {
                    if let Some(f) = d.push(s) {
                        n += 1;
                        v += usize::from(f.voiced);
                    }
                }
                100.0 * v as f32 / n.max(1) as f32
            };
            // How far the output's pitch sits from the nearest semitone.
            let mut t = Tuner::new(sr, 2_048);
            let (mut offs, mut offs_in) = (Vec::new(), Vec::new());
            let lat = a.latency();
            for i in (0..l.len().saturating_sub(lat + 512)).step_by(512) {
                t.push(&take.left[i..i + 512]);
                let p_in = t.detect().filter(|p| p.clarity > 0.9);
                if let Some(p) = p_in {
                    offs_in.push(p.cents.abs());
                }
            }
            let mut t = Tuner::new(sr, 2_048);
            for i in (lat..l.len() - 512).step_by(512) {
                t.push(&l[i..i + 512]);
                if let Some(p) = t.detect().filter(|p| p.clarity > 0.9) {
                    offs.push(p.cents.abs());
                }
            }
            let stereo: Vec<f32> = l.iter().zip(&r).flat_map(|(&a, &b)| [a, b]).collect();
            crate::wav::write_wav_i16(format!("/tmp/autotune_{file}.wav"), sr as u32, 2, &stereo).unwrap();
            eprintln!(
                "{file}: voiced {voiced:.0} % of frames; off the nearest semitone: input median {:.1} ct, output median {:.1} ct (p90 {:.1}) -> /tmp/autotune_{file}.wav",
                percentile(offs_in.clone(), 0.5),
                percentile(offs.clone(), 0.5),
                percentile(offs, 0.9)
            );
        }
    }
}

/// Frames of a voice held at `midi`, for the corrector alone.
fn corrector_run(s: &Settings, midi: impl Fn(f32) -> f32, secs: f32, held: &ChannelNotes) -> Vec<Corrected> {
    let mut c = Corrector::new(0.002);
    let n = (secs / 0.002) as usize;
    (0..n).map(|i| c.process(s, ponzik_dsp::util::midi_to_hz(midi(i as f32 * 0.002)), true, held)).collect()
}

#[test]
fn retune_speed_matches_the_knob() {
    // A voice 40 cents flat of A4: the output reaches 90 % of the way to A4 in the retune time.
    let mut report = Vec::new();
    for retune in [0.0, 0.01, 0.05, 0.1, 0.2, 0.4] {
        let s = Settings { retune, ..Settings::default() };
        let out = corrector_run(&s, |_| 68.6, 1.0, &ChannelNotes::EMPTY);
        let at = out.iter().position(|c| c.output >= 68.6 + 0.9 * 0.4).unwrap() as f32 * 0.002;
        report.push(format!("retune {:.0} ms: 90 % after {:.1} ms", retune * 1000.0, at * 1000.0));
        assert!((at - retune).abs() <= 0.1 * retune + 0.0021, "{}", report.last().unwrap());
        assert!((out.last().unwrap().output - 69.0).abs() < 0.005);
    }
    // End to end, on the audio: 100 ms takes about that long to land.
    let notes = [note(0.1, 1.2, 68.6)];
    let x = sing(SR, &notes, &Style { vibrato_cents: 0.0, drift_cents: 0.0, ..Style::default() }, 1.3);
    let mut a = tune(&[(P_RETUNE, 0.1)]);
    let y = process(&mut a, &x);
    let lat = a.latency() as f32 / SR;
    let pts = measured(&y, 1_024, 128);
    let at = |t: f32| pts.iter().min_by(|a, b| (a.0 - lat - t).abs().total_cmp(&(b.0 - lat - t).abs())).unwrap().1;
    let (early, late) = (at(0.13), at(0.5));
    report.push(format!("audio, 100 ms: {:.2} at 30 ms, {:.2} at 400 ms after the onset", early, late));
    assert!(early < 68.85 && (late - 69.0).abs() < 0.03, "{}", report.last().unwrap());
    eprintln!("{}", report.join("\n"));
}

#[test]
fn humanize_flex_tune_and_vibrato_keep_expression() {
    let vib = |t: f32| 69.1 + 0.4 * (std::f32::consts::TAU * 5.5 * t).sin();
    let depth = |out: &[Corrected]| {
        let tail = &out[out.len() / 2..];
        let (lo, hi) = tail.iter().fold((f32::MAX, f32::MIN), |(lo, hi), c| (lo.min(c.output), hi.max(c.output)));
        (hi - lo) * 50.0
    };
    let none = ChannelNotes::EMPTY;
    let hard = depth(&corrector_run(&Settings { retune: 0.0, ..Settings::default() }, vib, 2.0, &none));
    let human = depth(&corrector_run(&Settings { retune: 0.0, humanize: 1.0, ..Settings::default() }, vib, 2.0, &none));
    let slow = depth(&corrector_run(&Settings { retune: 0.2, ..Settings::default() }, vib, 2.0, &none));
    let less = depth(&corrector_run(&Settings { retune: 0.2, natural_vibrato: 0.25, ..Settings::default() }, vib, 2.0, &none));
    let made = depth(&corrector_run(&Settings { retune: 0.0, vib_depth: 0.3, ..Settings::default() }, vib, 2.0, &none));
    eprintln!("vibrato depth (cents): hard {hard:.1}, humanize {human:.1}, slow {slow:.1}, natural vibrato -12 dB {less:.1}, created 30 {made:.1}");
    assert!(hard < 1.0 && human > 10.0 && slow > 30.0 && less < 0.4 * slow && (made - 30.0).abs() < 3.0);
    // Flex-Tune: a note sung 45 cents off is left alone, one 5 cents off is tuned.
    let s = Settings { retune: 0.0, flex: 0.8, ..Settings::default() };
    let far = corrector_run(&s, |_| 69.45, 0.5, &none).last().unwrap().output;
    let near = corrector_run(&s, |_| 69.05, 0.5, &none).last().unwrap().output;
    assert!((far - 69.45).abs() < 0.01 && (near - 69.0).abs() < 0.01, "{far} {near}");
}

#[test]
fn scale_notes_bypass_and_midi_targets() {
    let mut get = [0.0f32; PARAM_COUNT];
    get[P_KEY as usize] = 9.0;
    get[P_SCALE as usize] = 1.0; // A minor
    get[(P_NOTE + 7) as usize] = NOTE_OFF as f32; // no G
    get[(P_NOTE + 11) as usize] = NOTE_BYPASS as f32; // B left alone
    let (allowed, bypass) = note_masks(&|id| get[id as usize]);
    assert_eq!(allowed, [0, 2, 4, 5, 9].iter().fold(0, |m, pc| m | 1 << pc));
    assert_eq!(bypass, 1 << 11);
    let s = Settings { retune: 0.0, allowed, bypass, ..Settings::default() };
    let none = ChannelNotes::EMPTY;
    // G removed: a G goes to A or F, the nearer one.
    assert_eq!(corrector_run(&s, |_| 67.3, 0.2, &none).last().unwrap().note, Some(69));
    // B bypassed: sung 30 cents sharp, it stays so.
    let b = corrector_run(&s, |_| 71.3, 0.2, &none);
    assert!(b.last().unwrap().bypassed && (b.last().unwrap().output - 71.3).abs() < 0.01);
    // MIDI melody: the held note is the target, in the voice's octave.
    let mut held = ChannelNotes::EMPTY;
    held.note_on(50);
    let m = Settings { midi: MidiTarget::Melody, ..s };
    assert!((corrector_run(&m, |_| 61.6, 0.2, &held).last().unwrap().output - 62.0).abs() < 0.01);
    assert!(corrector_run(&m, |_| 61.6, 0.2, &none).last().unwrap().bypassed);
    // MIDI notes: the held chord is the scale.
    held.note_on(53);
    let n = Settings { midi: MidiTarget::Notes, ..s };
    assert_eq!(corrector_run(&n, |_| 64.2, 0.2, &held).last().unwrap().note, Some(65));
}

#[test]
fn midi_melody_drives_the_effect() {
    let x = sing(SR, &[note(0.05, 1.0, 64.3)], &Style::default(), 1.05);
    let mut a = tune(&[(P_RETUNE, 0.0), (P_MIDI_MODE, 2.0), (P_MIDI_CHANNEL, 2.0)]);
    let mut held = [ChannelNotes::EMPTY; 3];
    held[1].note_on(67);
    let (mut l, mut r) = (x.clone(), x.clone());
    for (cl, cr) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
        a.channel_notes(&held);
        a.process(cl, cr);
    }
    let pts = measured(&l[24_000..], 1_024, 256);
    let mean = pts.iter().map(|p| p.1).sum::<f32>() / pts.len() as f32;
    assert!((mean - 67.0).abs() < 0.05, "{mean}");
}

#[test]
fn unvoiced_noise_passes_untouched() {
    // White noise and sibilants between notes: the output is the input, delayed.
    for mode in [0.0, 1.0] {
        let mut a = tune(&[(P_RETUNE, 0.0), (P_MODE, mode), (P_TRANSPOSE, 5.0)]);
        let x = noise(0.3, 48_000, 3);
        let y = process(&mut a, &x);
        let lat = a.latency();
        let err: Vec<f32> = (lat..x.len()).map(|i| y[i] - x[i - lat]).collect();
        let residual = 20.0 * (rms(&err) / rms(&x)).log10();
        eprintln!("{}: noise residual {residual:.1} dB", MODES[mode as usize]);
        assert!(residual < -60.0, "{residual}");
    }
    let notes = [note(0.05, 0.3, 57.4), Sung { start: 0.35, dur: 0.15, midi: f32::NAN, vowel: 0 }, note(0.5, 0.3, 59.7)];
    let x = sing(SR, &notes, &Style::default(), 0.85);
    let mut a = tune(&[(P_RETUNE, 0.0)]);
    let y = process(&mut a, &x);
    let lat = a.latency();
    let (s0, s1) = ((0.39 * SR) as usize, (0.47 * SR) as usize);
    let err: Vec<f32> = (s0..s1).map(|i| y[i + lat] - x[i]).collect();
    let residual = 20.0 * (rms(&err) / rms(&x[s0..s1])).log10();
    eprintln!("sibilant between notes: residual {residual:.1} dB");
    assert!(residual < -40.0, "{residual}");
}

/// Largest second difference, the mark of a click.
fn spikes(x: &[f32]) -> f32 {
    x.windows(3).map(|w| (w[2] - 2.0 * w[1] + w[0]).abs()).fold(0.0, f32::max)
}

#[test]
fn no_clicks_at_note_changes() {
    let (notes, _) = melody(55.0);
    let x = sing(SR, &notes, &Style::default(), 3.9);
    let dry = spikes(&x);
    for (mode, retune) in [(0.0, 0.0), (0.0, 0.05), (1.0, 0.0)] {
        let mut a = tune(&[C_MAJOR[0], C_MAJOR[1], C_MAJOR[2], (P_RETUNE, retune), (P_MODE, mode), (P_RANGE, 2.0)]);
        let y = process(&mut a, &x);
        let wet = spikes(&y);
        eprintln!("{} retune {:.0} ms: second difference {:.4} (dry {:.4}), ratio {:.2}", MODES[mode as usize], retune * 1000.0, wet, dry, wet / dry);
        assert!(wet < 1.6 * dry, "{wet} vs {dry}");
    }
}

/// Formants (Hz) of `x` around `at` by LPC (order 18 at 12 kHz).
pub(crate) fn formants(x: &[f32], at: usize) -> Vec<f32> {
    // Decimate to 12 kHz after a crude lowpass (4-sample mean).
    let n = 1_024;
    let seg: Vec<f64> = (0..n).map(|i| x[at + 4 * i..at + 4 * i + 4].iter().map(|&v| f64::from(v)).sum::<f64>() / 4.0).collect();
    let w: Vec<f64> = seg.iter().enumerate().map(|(i, v)| v * (0.54 - 0.46 * (std::f64::consts::TAU * i as f64 / (n - 1) as f64).cos())).collect();
    let order = 18;
    let r: Vec<f64> = (0..=order).map(|k| (k..n).map(|i| w[i] * w[i - k]).sum()).collect();
    let mut a = vec![0.0f64; order + 1];
    a[0] = 1.0;
    let mut e = r[0] * (1.0 + 1e-9);
    for i in 1..=order {
        let acc: f64 = (1..i).map(|j| a[j] * r[i - j]).sum();
        let k = -(r[i] + acc) / e;
        let prev = a.clone();
        for j in 1..i {
            a[j] = prev[j] + k * prev[i - j];
        }
        a[i] = k;
        e *= 1.0 - k * k;
    }
    let rate = f64::from(SR) / 4.0;
    let mag = |hz: f64| {
        let (mut re, mut im) = (0.0, 0.0);
        for (k, c) in a.iter().enumerate() {
            let ph = -std::f64::consts::TAU * hz * k as f64 / rate;
            re += c * ph.cos();
            im += c * ph.sin();
        }
        -(re * re + im * im).ln()
    };
    let grid: Vec<f64> = (0..400).map(|i| 150.0 + i as f64 * 10.0).collect();
    let m: Vec<f64> = grid.iter().map(|&f| mag(f)).collect();
    (1..m.len() - 1).filter(|&i| m[i] > m[i - 1] && m[i] >= m[i + 1]).map(|i| grid[i] as f32).collect()
}

/// The formant nearest `hz`.
pub(crate) fn near(f: &[f32], hz: f32) -> f32 {
    f.iter().copied().min_by(|a, b| (a - hz).abs().total_cmp(&(b - hz).abs())).unwrap_or(0.0)
}

#[test]
fn formants_stay_put_when_the_pitch_moves() {
    let notes = [Sung { start: 0.02, dur: 1.0, midi: 55.0, vowel: 1 }];
    let x = sing(SR, &notes, &Style { vibrato_cents: 0.0, drift_cents: 0.0, ..Style::default() }, 1.0);
    let at = (0.5 * SR) as usize;
    let f_in = formants(&x, at);
    eprintln!("input LPC peaks {f_in:?}");
    let (f1, f2) = (near(&f_in, 530.0), near(&f_in, 1_840.0));
    let mut report = vec![format!("input: F1 {f1:.0} Hz, F2 {f2:.0} Hz")];
    for (shift, preserve) in [(5.0, 1.0), (-5.0, 1.0), (5.0, 0.0), (-5.0, 0.0)] {
        let mut a = tune(&[(P_RETUNE, 0.0), (P_TRANSPOSE, shift), (P_FORMANT, preserve), (P_RANGE, 3.0)]);
        let y = process(&mut a, &x);
        let f = formants(&y, at + a.latency());
        let ratio = 2f32.powf(shift / 12.0);
        let expect = if preserve > 0.5 { 1.0 } else { ratio };
        let (g1, g2) = (near(&f, f1 * expect), near(&f, f2 * expect));
        let (e1, e2) = ((g1 / (f1 * expect) - 1.0) * 100.0, (g2 / (f2 * expect) - 1.0) * 100.0);
        report.push(format!(
            "{shift:+} st, formants {}: F1 {g1:.0} Hz ({e1:+.1} %), F2 {g2:.0} Hz ({e2:+.1} %)",
            if preserve > 0.5 { "preserved" } else { "follow" }
        ));
        assert!(e1.abs() < 6.0 && e2.abs() < 6.0, "{}", report.last().unwrap());
        // And the pitch did move.
        let pts = measured(&y[at..at + 9_600], 1_024, 512);
        let pitch = pts.iter().map(|p| p.1).sum::<f32>() / pts.len() as f32;
        assert!((pitch - 55.0 - shift).abs() < 0.05, "{pitch}");
    }
    eprintln!("{}", report.join("\n"));
}

#[test]
fn level_holds_while_shifting() {
    let notes = [note(0.02, 1.0, 57.0)];
    let x = sing(SR, &notes, &Style { vibrato_cents: 0.0, drift_cents: 0.0, ..Style::default() }, 1.0);
    let at = (0.3 * SR) as usize..(0.9 * SR) as usize;
    for shift in [-7.0, -2.0, 2.0, 7.0, 12.0] {
        let mut a = tune(&[(P_RETUNE, 0.0), (P_TRANSPOSE, shift)]);
        let y = process(&mut a, &x);
        let db = 20.0 * (rms(&y[at.clone()]) / rms(&x[at.clone()])).log10();
        eprintln!("{shift:+} st: {db:+.2} dB");
        assert!(db.abs() < 0.5, "{shift}: {db}");
    }
}

#[test]
fn reported_latency_is_the_real_one() {
    // Unvoiced and voiced (in tune, nothing to correct): the output is the input `latency()` later.
    let notes = [note(0.02, 0.9, 57.0)];
    let voice = sing(SR, &notes, &Style { vibrato_cents: 0.0, drift_cents: 0.0, ..Style::default() }, 1.0);
    for mode in [0.0, 1.0] {
        for latency in [0.0, 1.0] {
            for (what, x) in [("noise", noise(0.3, 24_000, 9)), ("voice", voice.clone())] {
                let mut a = tune(&[(P_MODE, mode), (P_LATENCY, latency), (P_RANGE, 2.0)]);
                let y = process(&mut a, &x);
                // The voice repeats every period: its peak is looked for near the latency.
                let lat = a.latency();
                let range = if what == "voice" { lat - 100..lat + 100 } else { 0..2_400 };
                let best = range
                    .max_by(|&p, &q| {
                        let c = |lag: usize| (12_000..20_000).map(|i| y[i + lag] * x[i]).sum::<f32>();
                        c(p).total_cmp(&c(q))
                    })
                    .unwrap();
                eprintln!("{} {} {what}: latency() {} samples, measured {best}", MODES[mode as usize], LATENCIES[latency as usize], a.latency());
                let tol = if mode > 0.5 && what == "voice" { a.detector.longest_period() as usize / 2 + 2 } else { 1 };
                assert!(best.abs_diff(a.latency()) <= tol, "{best} vs {}", a.latency());
            }
        }
    }
}

#[test]
fn stable_at_extremes() {
    assert_stable_at_extremes(|| Box::new(AutoTune::default()), 4.0);
    // Extreme voice settings on a sung line, every mode.
    let (notes, _) = melody(48.0);
    let x = sing(SR, &notes, &Style::default(), 2.0);
    for mode in [0.0, 1.0] {
        let mut a = tune(&[(P_MODE, mode), (P_TRANSPOSE, 12.0), (P_FORMANT_SHIFT, -12.0), (P_THROAT, 1.8), (P_WIDTH, 30.0), (P_VIB_DEPTH, 100.0), (P_VIB_RATE, 10.0)]);
        let y = process(&mut a, &x);
        assert!(y.iter().all(|s| s.is_finite() && s.abs() < 4.0));
        // Silence after: back to exact zeros, nothing subnormal.
        let z = process(&mut a, &vec![0.0; 48_000]);
        assert!(z[24_000..].iter().all(|&s| s == 0.0 || s.abs() > 1e-30));
    }
}
