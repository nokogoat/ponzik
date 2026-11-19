//! Measurements of the model: what an ear would check, with numbers.

use ponzik_dsp::fft::{RealFft, hann};

use super::super::common::test_util::stress;
use super::*;

const SR: f32 = 48_000.0;

fn piano() -> Piano {
    let mut p = Piano::new();
    p.set_sample_rate(SR);
    p
}

/// Only the strings: no noises, no room, no sympathetic strings.
fn dry() -> Piano {
    let mut p = piano();
    for (id, v) in [(P_HAMMER_NOISE, 0.0), (P_RELEASE_NOISE, 0.0), (P_PEDAL_NOISE, 0.0), (P_ROOM, 0.0), (P_RESONANCE, 0.0)] {
        p.set_param(id, v);
    }
    p
}

fn run(p: &mut Piano, frames: usize) -> (Vec<f32>, Vec<f32>) {
    let mut l = vec![0.0; frames];
    let mut r = vec![0.0; frames];
    for (a, b) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
        p.render(a, b);
    }
    (l, r)
}

fn mono(p: &mut Piano, frames: usize) -> Vec<f32> {
    let (l, r) = run(p, frames);
    l.iter().zip(&r).map(|(a, b)| 0.5 * (a + b)).collect()
}

fn rms(s: &[f32]) -> f32 {
    (s.iter().map(|x| x * x).sum::<f32>() / s.len().max(1) as f32).sqrt()
}

/// Complex spectrum (Hann window) of `n` samples from `at`.
fn complex_spectrum(s: &[f32], at: usize, n: usize) -> (Vec<f32>, Vec<f32>) {
    let mut w = vec![0.0; n];
    hann(&mut w);
    let x: Vec<f32> = s[at..at + n].iter().zip(&w).map(|(a, b)| a * b).collect();
    let mut fft = RealFft::new(n);
    let (mut re, mut im) = (vec![0.0; n / 2 + 1], vec![0.0; n / 2 + 1]);
    fft.forward(&x, &mut re, &mut im);
    (re, im)
}

/// Magnitude spectrum (Hann window) of the first `n` samples.
fn spectrum(s: &[f32], n: usize) -> Vec<f32> {
    let (re, im) = complex_spectrum(s, 0, n);
    re.iter().zip(&im).map(|(a, b)| a.hypot(*b)).collect()
}

/// FFT size of the frequency measurements.
const FFT: usize = 16_384;

/// Frequency of the strongest peak within `tol` Hz of `hz`, refined by the
/// phase advance between two frames a quarter of a frame apart.
fn peak_near(s: &[f32], hz: f32, tol: f32) -> f32 {
    let hop = FFT / 4;
    let (r1, i1) = complex_spectrum(s, 0, FFT);
    let (r2, i2) = complex_spectrum(s, hop, FFT);
    let bin = SR / FFT as f32;
    let lo = ((hz - tol) / bin).floor().max(1.0) as usize;
    let hi = (((hz + tol) / bin).ceil() as usize).min(r1.len() - 2);
    let k = (lo..=hi).max_by(|&a, &b| r1[a].hypot(i1[a]).total_cmp(&r1[b].hypot(i1[b]))).unwrap_or(lo);
    let tau = std::f64::consts::TAU;
    let expected = tau * k as f64 * hop as f64 / FFT as f64;
    let dphi = (i2[k] as f64).atan2(r2[k] as f64) - (i1[k] as f64).atan2(r1[k] as f64) - expected;
    let dev = dphi - tau * (dphi / tau).round();
    ((k as f64 + dev * FFT as f64 / (tau * hop as f64)) * bin as f64) as f32
}

/// RMS envelope in dB, frames of `hop` samples.
fn envelope_db(s: &[f32], hop: usize) -> Vec<f32> {
    s.chunks(hop).map(|c| 20.0 * rms(c).max(1e-12).log10()).collect()
}

/// Slope (dB/s) of the least-squares line through `env` between `t0` and `t1` s.
fn slope(env: &[f32], hop: usize, t0: f32, t1: f32) -> f32 {
    let dt = hop as f32 / SR;
    let pts: Vec<(f32, f32)> =
        env.iter().enumerate().map(|(i, &v)| (i as f32 * dt, v)).filter(|&(t, _)| t >= t0 && t <= t1).collect();
    let n = pts.len() as f32;
    let (mx, my) = (pts.iter().map(|p| p.0).sum::<f32>() / n, pts.iter().map(|p| p.1).sum::<f32>() / n);
    let sxy: f32 = pts.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum();
    let sxx: f32 = pts.iter().map(|p| (p.0 - mx).powi(2)).sum();
    sxy / sxx
}

/// T60 from the decay between `t0` and `t1`.
fn t60(note: u8, t0: f32, t1: f32) -> f32 {
    let mut p = dry();
    p.note_on(note, 0.8);
    let s = mono(&mut p, (t1 * SR) as usize + 4_800);
    let env = envelope_db(&s, 2_400);
    -60.0 / slope(&env, 2_400, t0, t1)
}

fn centroid(s: &[f32]) -> f32 {
    let n = 16_384;
    let mag = spectrum(s, n);
    let bin = SR / n as f32;
    let (num, den) = mag.iter().enumerate().fold((0.0, 0.0), |(a, b), (k, &m)| (a + k as f32 * bin * m * m, b + m * m));
    num / den
}

#[test]
fn plays_and_releases() {
    let mut p = piano();
    assert_eq!(run(&mut p, 512).0.iter().fold(0.0f32, |m, x| m.max(x.abs())), 0.0);
    p.note_on(72, 0.8);
    let (l, _) = run(&mut p, 4_800);
    assert!(l.iter().fold(0.0f32, |m, x| m.max(x.abs())) > 0.02);
    p.note_off(72);
    run(&mut p, 48_000 * 2);
    assert_eq!(p.active_voices(), 0, "the damped string still sounds");
    // Then the case falls silent too, and the piano costs nothing.
    run(&mut p, 48_000 * 8);
    let (l, r) = run(&mut p, 4_800);
    assert!(l.iter().chain(&r).all(|&x| x == 0.0));
}

/// The partials of the stiff strings follow f_n = n·f₀·√(1 + B·n²).
#[test]
fn partials_follow_inharmonicity() {
    for (note, partials) in [(33u8, 24usize), (45, 16), (60, 10), (84, 4)] {
        let mut p = dry();
        p.set_param(P_UNISON, 0.0);
        p.set_param(P_BOARD, 0.0);
        p.note_on(note, 0.9);
        let s = mono(&mut p, FFT * 2 + 2_400);
        let s = &s[2_400..];
        let k = table_key(note);
        let b = keys::inharmonicity(k, 1.0);
        let f1 = peak_near(s, p.f1[note as usize], 3.0);
        assert!((f1 / p.f1[note as usize] - 1.0).abs() < 1e-3, "{note}: f1 {f1} vs {}", p.f1[note as usize]);
        for i in 2..=partials {
            let n_ = i as f32;
            let want = f1 * keys::partial_ratio(n_, b);
            let got = peak_near(s, want, want * 0.003 + 3.0);
            let cents = 1200.0 * (got / want).log2();
            assert!(cents.abs() < 2.0, "note {note} partial {i}: {got} Hz, expected {want} ({cents:+.2} ct)");
        }
        // Clearly away from the harmonic series at the top.
        let n_ = partials as f32;
        let sharp = 1200.0 * (keys::partial_ratio(n_, b) / n_).log2();
        assert!(sharp > 5.0, "note {note}: partial {partials} only {sharp} ct sharp");
    }
}

#[test]
fn decay_times_per_register() {
    let bass = t60(24, 3.0, 12.0);
    let low_mid = t60(48, 2.0, 8.0);
    let treble = t60(96, 0.3, 1.5);
    let top = t60(105, 0.2, 1.0);
    println!("T60: C1 {bass:.1} s, C3 {low_mid:.1} s, C7 {treble:.2} s, A7 {top:.2} s");
    assert!((20.0..=40.0).contains(&bass), "bass T60 {bass}");
    assert!((10.0..=30.0).contains(&low_mid), "C3 T60 {low_mid}");
    assert!((1.0..=3.0).contains(&treble), "treble T60 {treble}");
    assert!(top < treble && top > 0.5, "top T60 {top}");
}

/// Strength of partial `n` of `note` over time (Goertzel, frames of `hop`).
fn partial_envelope(s: &[f32], hz: f32, hop: usize) -> Vec<f32> {
    let w = std::f32::consts::TAU * hz / SR;
    let coef = 2.0 * w.cos();
    let mut win = vec![0.0; hop];
    hann(&mut win);
    s.chunks_exact(hop)
        .map(|c| {
            let (mut s1, mut s2) = (0.0f32, 0.0f32);
            for (&x, &w) in c.iter().zip(&win) {
                let s0 = x * w + coef * s1 - s2;
                s2 = s1;
                s1 = s0;
            }
            let p = s1 * s1 + s2 * s2 - coef * s1 * s2;
            10.0 * p.max(1e-30).log10()
        })
        .collect()
}

/// Ripple of an envelope: RMS distance (dB) to its moving average.
fn ripple(env: &[f32], half: usize) -> f32 {
    let mut sum = 0.0;
    let mut n = 0;
    for i in half..env.len() - half {
        let avg = env[i - half..=i + half].iter().sum::<f32>() / (2 * half + 1) as f32;
        sum += (env[i] - avg).powi(2);
        n += 1;
    }
    (sum / n as f32).sqrt()
}

/// Mistuned unison strings beat, and the decay has two stages: a fast
/// prompt sound, then a slow aftersound.
#[test]
fn unisons_beat_and_decay_in_two_stages() {
    let measure = |unison: f32| {
        let mut p = dry();
        p.set_param(P_UNISON, unison);
        p.note_on(60, 0.8);
        let s = mono(&mut p, 48_000 * 8);
        let b = keys::inharmonicity(60, 1.0);
        let hz = p.f1[60] * keys::partial_ratio(5.0, b);
        let env = partial_envelope(&s[4_800..], hz, 2_400);
        let broad = envelope_db(&s, 2_400);
        (ripple(&env, 6), slope(&broad, 2_400, 0.1, 1.5), slope(&broad, 2_400, 4.0, 8.0))
    };
    let (beats, early, late) = measure(2.0);
    let (flat, ..) = measure(0.0);
    println!("ripple {beats:.2} dB vs {flat:.2} dB; decay {early:.1} then {late:.1} dB/s");
    assert!(beats > 1.0 && beats > 2.5 * flat, "beating {beats} vs {flat}");
    assert!(early < 1.8 * late, "no prompt sound: {early} then {late} dB/s");
}

/// Harder strikes are brighter, not only louder: the felt stiffens.
#[test]
fn velocity_raises_brightness() {
    let measure = |vel: f32| {
        let mut p = dry();
        p.note_on(60, vel);
        let s = mono(&mut p, 19_200);
        (rms(&s), centroid(&s))
    };
    let (soft_level, soft) = measure(0.3);
    let (mid_level, mid) = measure(0.65);
    let (hard_level, hard) = measure(1.0);
    println!("centroid {soft:.0} → {mid:.0} → {hard:.0} Hz; level {soft_level:.4} → {hard_level:.4}");
    assert!(mid > soft * 1.05 && hard > mid * 1.05, "{soft} {mid} {hard}");
    assert!(hard > 1.3 * soft);
    assert!(hard_level > 4.0 * soft_level && mid_level > soft_level);
    // A harder felt is brighter at the same velocity.
    let mut p = dry();
    p.set_param(P_HARDNESS, 1.0);
    p.note_on(60, 0.65);
    let bright = centroid(&mono(&mut p, 19_200));
    assert!(bright > mid * 1.1, "hardness: {mid} → {bright}");
}

#[test]
fn sustain_pedal_sets_the_other_strings_ringing() {
    let energy = |pedal: f32, resonance: f32| {
        let mut p = piano();
        p.set_param(P_RESONANCE, resonance);
        p.set_param(P_PEDAL_NOISE, 0.0);
        p.controller(Controller::Cc(64, pedal));
        run(&mut p, 4_800);
        p.resonance.total = 0.0;
        p.note_on(48, 0.8);
        p.note_on(55, 0.8);
        run(&mut p, 24_000);
        p.note_off(48);
        p.note_off(55);
        let s = run(&mut p, 48_000);
        (p.resonance.total, rms(&s.0))
    };
    let (up, _) = energy(0.0, 1.0);
    let (down, _) = energy(1.0, 1.0);
    let (half, _) = energy(0.45, 1.0);
    println!("sympathetic energy: pedal up {up:.3e}, half {half:.3e}, down {down:.3e}");
    assert!(down > 10.0 * up, "{up} → {down}");
    assert!(half > up && half < down, "half pedal {half}");
    // The resonance knob sets how much is heard.
    let (_, with) = energy(1.0, 1.0);
    let (_, without) = energy(1.0, 0.0);
    assert!(with > without * 1.02, "{without} → {with}");
}

/// Released keys are damped, except under the pedal and in the top octave
/// and a half, which has no dampers; half-pedaling damps partly.
#[test]
fn dampers_pedal_and_top_keys() {
    // Level 0.4 s after the release, over the level of the key held on.
    let after_release = |note: u8, pedal: f32| {
        let level = |release: bool| {
            let mut p = dry();
            p.controller(Controller::Cc(64, pedal));
            p.note_on(note, 0.8);
            run(&mut p, 24_000);
            if release {
                p.note_off(note);
            }
            rms(&mono(&mut p, 24_000)[19_200..])
        };
        level(true) / level(false)
    };
    let damped = after_release(60, 0.0);
    let half = after_release(60, 0.45);
    let pedaled = after_release(60, 1.0);
    let top = after_release(100, 0.0);
    println!("level 0.4 s after release: damped {damped:.4}, half {half:.4}, pedal {pedaled:.3}, top key {top:.3}");
    assert!(damped < 0.01, "{damped}");
    assert!((pedaled - 1.0).abs() < 0.05, "{pedaled}");
    assert!(half > 2.0 * damped && half < 0.7 * pedaled, "{half}");
    assert!((top - 1.0).abs() < 0.05, "the top keys have no dampers: {top}");
}

#[test]
fn sostenuto_holds_only_the_keys_down() {
    let mut p = dry();
    p.note_on(48, 0.8);
    run(&mut p, 2_400);
    p.controller(Controller::Cc(66, 1.0));
    p.note_off(48);
    p.note_on(64, 0.8);
    run(&mut p, 2_400);
    p.note_off(64);
    run(&mut p, 48_000);
    let alive = |p: &Piano, k: u8| p.voices.iter().any(|v| v.active && v.key == k && v.damper < 0.5);
    assert!(alive(&p, 48), "the sostenuto lets the held key ring");
    assert!(!p.voices.iter().any(|v| v.active && v.key == 64 && v.level > 1e-6), "the later key is damped");
    p.controller(Controller::Cc(66, 0.0));
    run(&mut p, 4_800);
    assert!(!alive(&p, 48));
}

#[test]
fn una_corda_is_softer_and_mellower() {
    let measure = |soft: f32| {
        let mut p = dry();
        p.controller(Controller::Cc(67, soft));
        p.note_on(67, 0.8);
        let s = mono(&mut p, 19_200);
        (rms(&s), centroid(&s))
    };
    let (level, bright) = measure(0.0);
    let (soft_level, soft_bright) = measure(1.0);
    println!("una corda: level {level:.4} → {soft_level:.4}, centroid {bright:.0} → {soft_bright:.0}");
    assert!(soft_level < 0.9 * level && soft_bright < 0.95 * bright);
}

#[test]
fn strikes_reuse_keys_and_steal_quiet_voices() {
    let mut p = piano();
    p.note_on(60, 0.8);
    run(&mut p, 256);
    p.note_on(60, 0.5);
    assert_eq!(p.active_voices(), 1, "a key restrikes its own strings");
    p.controller(Controller::Cc(64, 1.0));
    for n in 21..=108u8 {
        p.note_on(n, 0.3 + (n % 5) as f32 * 0.1);
        run(&mut p, 128);
        p.note_off(n);
    }
    assert_eq!(p.active_voices(), VOICES);
    // The last keys play; what was stolen was quiet.
    assert!(p.voices.iter().any(|v| v.active && v.key == 108));
    let (l, r) = run(&mut p, 9_600);
    assert!(l.iter().chain(&r).all(|x| x.is_finite() && x.abs() < 2.0));
}

/// Re-striking a sounding key hits the moving strings: no click, no restart
/// from silence.
#[test]
fn restrike_and_release_have_no_clicks() {
    let mut p = dry();
    p.controller(Controller::Cc(64, 0.0));
    let mut s = Vec::new();
    for i in 0..6 {
        p.note_on(57 + (i % 2) * 7, 0.4 + 0.1 * i as f32);
        s.extend(mono(&mut p, 6_000));
        p.note_off(57 + (i % 2) * 7);
        s.extend(mono(&mut p, 3_000));
    }
    // The largest step between samples stays a small part of the peak, as
    // for a smooth waveform of a few kilohertz.
    let peak = s.iter().fold(0.0f32, |m, x| m.max(x.abs()));
    let step = s.windows(2).fold(0.0f32, |m, w| m.max((w[1] - w[0]).abs()));
    println!("peak {peak:.3}, largest step {step:.4}");
    assert!(step < 0.5 * peak, "{step} vs {peak}");
}

#[test]
fn materials_and_variants_sound() {
    for m in 0..3 {
        let mut p = piano();
        p.set_param(P_MATERIAL, m as f32);
        p.set_param(P_FELT, if m == 0 { 1.0 } else { 0.0 });
        p.set_param(P_SIZE, 0.0);
        for n in [36u8, 60, 84] {
            p.note_on(n, 0.8);
        }
        let s = mono(&mut p, 24_000);
        let level = rms(&s);
        assert!(level > 0.005 && level < 0.5, "material {m}: {level}");
    }
    // The felt mutes.
    let level = |felt: f32| {
        let mut p = dry();
        p.set_param(P_FELT, felt);
        p.note_on(72, 0.8);
        let s = mono(&mut p, 48_000);
        (centroid(&s), rms(&s[24_000..]))
    };
    let (open, open_tail) = level(0.0);
    let (muted, muted_tail) = level(1.0);
    assert!(muted < 0.85 * open && muted_tail < open_tail, "{open} → {muted}");
}

#[test]
fn tuning_follows_a4() {
    let mut p = dry();
    p.set_param(P_TUNING, 432.0);
    p.note_on(69, 0.8);
    let s = mono(&mut p, FFT * 2);
    let f = peak_near(&s, 432.0, 6.0);
    assert!((f - 432.0).abs() < 0.3, "{f}");
}

#[test]
fn no_nan_no_denormals_and_sane_extremes() {
    let mut p = piano();
    stress(&mut p);
    // A long tail decays to exact silence, never through denormals.
    let mut p = piano();
    p.controller(Controller::Cc(64, 1.0));
    for n in [21u8, 40, 64, 88, 108] {
        p.note_on(n, 1.0);
    }
    run(&mut p, 9_600);
    p.controller(Controller::Cc(64, 0.0));
    for n in [21u8, 40, 64, 88, 108] {
        p.note_off(n);
    }
    for _ in 0..12 {
        let (l, r) = run(&mut p, 48_000);
        assert!(l.iter().chain(&r).all(|x| x.is_finite() && (*x == 0.0 || x.abs() >= f32::MIN_POSITIVE)));
    }
    assert_eq!(p.active_voices(), 0);
}

#[test]
fn every_parameter_reads_back() {
    let mut p = piano();
    for info in p.params() {
        let v = info.from_norm(0.37);
        p.set_param(info.id, v);
        assert!((p.get_param(info.id) - v).abs() < 1e-3 * v.abs().max(1.0), "{}", info.name);
    }
}

