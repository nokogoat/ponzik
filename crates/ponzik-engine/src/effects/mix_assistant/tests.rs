use std::f32::consts::TAU;

use ponzik_dsp::biquad::{Biquad, BiquadCoefs};
use ponzik_dsp::noise::WhiteNoise;

use super::*;
use crate::effects::testing::*;

/// Bruit rose (filtre de Paul Kellet), de niveau efficace proche de `amp`.
fn pink(amp: f32, frames: usize, seed: u32) -> Vec<f32> {
    let mut w = WhiteNoise::new(seed);
    let mut b = [0.0f32; 7];
    (0..frames)
        .map(|_| {
            let x = w.next_sample();
            b[0] = 0.99886 * b[0] + x * 0.055_517_9;
            b[1] = 0.99332 * b[1] + x * 0.075_075_9;
            b[2] = 0.96900 * b[2] + x * 0.153_852;
            b[3] = 0.86650 * b[3] + x * 0.310_485_6;
            b[4] = 0.55000 * b[4] + x * 0.532_952_2;
            b[5] = -0.7616 * b[5] - x * 0.016_898;
            let y = b[0] + b[1] + b[2] + b[3] + b[4] + b[5] + b[6] + x * 0.5362;
            b[6] = x * 0.115926;
            y * amp * 0.11
        })
        .collect()
}

fn filtered(x: &[f32], coefs: &[BiquadCoefs]) -> Vec<f32> {
    let mut fs: Vec<Biquad> = coefs.iter().map(|&c| Biquad::new(c)).collect();
    x.iter().map(|&s| fs.iter_mut().fold(s, |s, f| f.process(s))).collect()
}

/// Écoute `x` (mono) jusqu'au bout et rend l'analyse.
fn listen(fx: &mut MixAssistant, x: &[f32]) -> Analysis {
    fx.set_param(P_LISTEN, 1.0);
    let before = fx.shared_state().generation();
    run_mono(fx, x);
    let shared = fx.shared_state();
    assert_eq!(shared.generation(), before + 1, "écoute inachevée");
    fx.set_param(P_LISTEN, 0.0);
    shared.analysis().expect("analyse publiée")
}

fn listener(seconds: f32) -> MixAssistant {
    let mut fx = MixAssistant::default();
    fx.set_sample_rate(SR);
    fx.set_listen_seconds(seconds);
    fx
}

fn apply(fx: &mut MixAssistant, s: &Suggestion) {
    for &(id, v) in &s.values {
        fx.set_param(id, v);
    }
}

/// Une suggestion qui fait tout travailler fort.
fn extreme(fx: &mut MixAssistant) {
    for (id, v) in [
        (P_HP_ON, 1.0),
        (P_HP_FREQ, 300.0),
        (P_BAND + 1, 12.0),
        (P_BAND + 4, -12.0),
        (P_BAND + 7, 9.0),
        (P_BAND + 13, 12.0),
        (P_DS_RANGE, 12.0),
        (P_DS_THRESHOLD, -60.0),
        (P_COMP_RATIO, 10.0),
        (P_COMP_THRESHOLD, -40.0),
        (P_COMP_MAKEUP, 18.0),
        (P_SAT_DRIVE, 1.0),
        (P_WIDTH, 2.0),
        (P_OUTPUT, 12.0),
    ] {
        fx.set_param(id, v);
    }
}

#[test]
fn neutral_by_default() {
    let mut fx = MixAssistant::default();
    let (l0, r0) = (noise(0.5, 9_600, 3), noise(0.5, 9_600, 4));
    let (mut l, mut r) = (l0.clone(), r0.clone());
    run(&mut fx, &mut l, &mut r);
    assert_eq!(l, l0);
    assert_eq!(r, r0);
}

#[test]
fn amount_zero_is_bit_transparent() {
    let mut fx = MixAssistant::default();
    extreme(&mut fx);
    fx.set_param(P_LISTEN, 1.0);
    fx.set_param(P_AMOUNT, 0.0);
    let (l0, r0) = (noise(0.5, 24_000, 3), noise(0.5, 24_000, 4));
    let (mut l, mut r) = (l0.clone(), r0.clone());
    run(&mut fx, &mut l, &mut r);
    assert_eq!(l, l0);
    assert_eq!(r, r0);
    // Le même réglage à 100 % change bien le son.
    fx.set_param(P_AMOUNT, 1.0);
    let (mut l, mut r) = (l0.clone(), r0.clone());
    run(&mut fx, &mut l, &mut r);
    assert_ne!(l, l0);
}

#[test]
fn amount_blends_toward_the_suggestion() {
    let level = |amount: f32| {
        let mut fx = MixAssistant::default();
        fx.set_param(P_OUTPUT, -12.0);
        fx.set_param(P_AMOUNT, amount);
        fx.reset();
        let (l, _) = run_mono(&mut fx, &sine(1_000.0, 0.5, 9_600));
        gain_to_db(rms(&l[4_800..]) / rms(&sine(1_000.0, 0.5, 9_600)[4_800..]))
    };
    assert!((level(1.0) + 12.0).abs() < 0.1);
    assert!((level(0.5) + 6.0).abs() < 0.1);
    assert!(level(0.0).abs() < 1e-4);
}

#[test]
fn stable() {
    assert_stable_at_extremes(|| Box::new(MixAssistant::default()), 1e4);
    let mut fx = MixAssistant::default();
    extreme(&mut fx);
    let (l, r) = run_mono(&mut fx, &noise(1.0, 48_000, 9));
    assert!(l.iter().chain(&r).all(|s| s.is_finite() && s.abs() < 100.0));
}

#[test]
fn listening_publishes_and_can_restart() {
    let mut fx = listener(1.0);
    let shared = fx.shared_state();
    assert!(shared.analysis().is_none());
    // Le silence ne fait pas avancer l'écoute.
    fx.set_param(P_LISTEN, 1.0);
    assert!(shared.listening());
    run_mono(&mut fx, &vec![0.0; 96_000]);
    assert_eq!(shared.generation(), 0);
    assert!(shared.progress() < 0.01);
    run_mono(&mut fx, &pink(0.2, 30_000, 1));
    assert!(shared.progress() > 0.5 && shared.progress() < 1.0, "{}", shared.progress());
    run_mono(&mut fx, &pink(0.2, 30_000, 2));
    assert_eq!(shared.generation(), 1);
    assert!(!shared.listening());
    fx.set_param(P_LISTEN, 0.0);
    listen(&mut fx, &pink(0.2, 60_000, 3));
    assert_eq!(shared.generation(), 2);
}

#[test]
fn bump_at_300_hz_leads_to_a_cut_there() {
    let x = pink(0.25, 48_000 * 5, 11);
    let x = filtered(&x, &[BiquadCoefs::peaking(300.0, 1.4, 6.0, SR)]);
    let mut fx = listener(4.5);
    let a = listen(&mut fx, &x);
    for profile in [Profile::Mix, Profile::Keys] {
        let s = suggest(&a, profile);
        let bells: Vec<(f32, f32)> = (1..4).map(|b| (s.get(P_BAND + 3 * b), s.get(P_BAND + 3 * b + 1))).collect();
        assert!(
            bells.iter().any(|&(hz, gain)| (220.0..420.0).contains(&hz) && gain <= -2.0),
            "{profile:?} : {bells:?}"
        );
        assert_eq!(s.get(P_EQ_ON), 1.0);
        // Sous 2 kHz, c'est la coupe la plus profonde.
        let deepest = bells.iter().filter(|b| b.0 < 2_000.0).min_by(|a, b| a.1.total_cmp(&b.1)).unwrap();
        assert!((220.0..420.0).contains(&deepest.0), "{profile:?} : {bells:?}");
    }
    // Sans la bosse, pas de coupe dans la zone.
    let mut fx = listener(4.5);
    let a = listen(&mut fx, &pink(0.25, 48_000 * 5, 12));
    let s = suggest(&a, Profile::Mix);
    for b in 1..4 {
        let (hz, gain) = (s.get(P_BAND + 3 * b), s.get(P_BAND + 3 * b + 1));
        assert!(!((220.0..420.0).contains(&hz) && gain <= -2.0), "{hz} {gain}");
    }
}

/// Bruit rose dont le niveau saute de segment en segment (±15 dB).
fn dynamic_signal(frames: usize) -> Vec<f32> {
    let x = pink(0.3, frames, 21);
    let seg = 4_800;
    x.iter()
        .enumerate()
        .map(|(n, &s)| {
            let k = n / seg;
            let g = match (k * 7 + k / 3) % 4 {
                0 => 1.0,
                1 => 0.03,
                2 => 0.5,
                _ => 0.06,
            };
            s * g
        })
        .collect()
}

#[test]
fn dynamic_input_gets_more_compression_than_a_flat_one() {
    let frames = 48_000 * 5;
    let flat = pink(0.1, frames, 21);
    let dynamic = dynamic_signal(frames);
    let measure = |x: &[f32]| {
        let mut fx = listener(4.5);
        let a = listen(&mut fx, x);
        let s = suggest(&a, Profile::Keys);
        apply(&mut fx, &s);
        // Réduction moyenne réellement appliquée, bloc par bloc.
        let shared = fx.shared_state();
        let (mut sum, mut n) = (0.0, 0);
        for chunk in x.chunks(256) {
            let (mut l, mut r) = (chunk.to_vec(), chunk.to_vec());
            fx.process(&mut l, &mut r);
            sum += shared.compressor_gr();
            n += 1;
        }
        (a, s.get(P_COMP_RATIO), sum / n as f32)
    };
    let (af, ratio_flat, gr_flat) = measure(&flat);
    let (ad, ratio_dyn, gr_dyn) = measure(&dynamic);
    assert!(ad.range_db > af.range_db + 10.0, "{} {}", ad.range_db, af.range_db);
    assert!(ratio_dyn > ratio_flat + 0.4, "{ratio_dyn} {ratio_flat}");
    assert!(gr_dyn > gr_flat + 1.5, "{gr_dyn} {gr_flat}");
}

/// Voyelle (harmoniques de 180 Hz) coupée toutes les 600 ms par un « s » de 150 ms.
fn sibilant_signal(frames: usize) -> Vec<f32> {
    let hiss = filtered(
        &noise(1.0, frames, 31),
        &[BiquadCoefs::highpass(5_500.0, 0.707, SR), BiquadCoefs::lowpass(9_000.0, 0.707, SR)],
    );
    (0..frames)
        .map(|n| {
            let t = n as f32 / SR;
            let vowel: f32 = (1..=12).map(|k| (TAU * 180.0 * k as f32 * t).sin() / k as f32).sum::<f32>() * 0.12;
            let in_s = (n % 28_800) >= 21_600;
            if in_s { 0.4 * hiss[n] + 0.2 * vowel } else { vowel }
        })
        .collect()
}

#[test]
fn sibilant_signal_triggers_the_de_esser_on_vocals() {
    let x = sibilant_signal(48_000 * 5);
    let mut fx = listener(4.5);
    let a = listen(&mut fx, &x);
    assert!(a.sibilance_db > -14.0, "{}", a.sibilance_db);
    let s = suggest(&a, Profile::Vocal);
    assert!(s.get(P_DS_RANGE) >= 3.0, "{s:?}");
    apply(&mut fx, &s);
    let shared = fx.shared_state();
    let (mut in_s, mut out_s) = (0.0f32, 0.0f32);
    for (k, chunk) in x.chunks(240).enumerate() {
        let (mut l, mut r) = (chunk.to_vec(), chunk.to_vec());
        fx.process(&mut l, &mut r);
        let pos = (k * 240) % 28_800;
        let gr = shared.deesser_gr();
        if (22_000..28_000).contains(&pos) {
            in_s = in_s.max(gr);
        } else if (9_600..20_000).contains(&pos) {
            out_s = out_s.max(gr);
        }
    }
    assert!(in_s > 3.0, "{in_s}");
    assert!(out_s < 1.0, "{out_s}");
    // Les autres profils ne proposent pas de de-esser.
    assert_eq!(suggest(&a, Profile::Keys).get(P_DS_RANGE), 0.0);
}

#[test]
fn detects_obvious_sources() {
    let mut fx = listener(2.0);
    let bass = sine(55.0, 0.5, 48_000 * 3);
    assert_eq!(detect(&listen(&mut fx, &bass)), Profile::Bass);
    let mut fx = listener(4.5);
    let a = listen(&mut fx, &sibilant_signal(48_000 * 5));
    assert_eq!(detect(&a), Profile::Vocal);
    assert_eq!(suggest(&a, Profile::Auto).profile, Profile::Vocal);
    // Des coups de bruit qui retombent : une batterie.
    let hits: Vec<f32> = noise(0.8, 48_000 * 8, 51)
        .iter()
        .enumerate()
        .map(|(n, s)| s * (-((n % 24_000) as f32) / 3_000.0).exp())
        .collect();
    let a = listen(&mut fx, &hits);
    assert!(a.attack_db > 14.0, "{}", a.attack_db);
    assert_eq!(detect(&a), Profile::Drums);
}

#[test]
fn suggested_output_is_sensible() {
    let mut fx = listener(4.5);
    // Une source discrète, vers −30 dBFS efficaces.
    let quiet = |frames, seed| {
        let x = pink(1.0, frames, seed);
        let g = 10f32.powf(-30.0 / 20.0) / rms(&x);
        x.iter().map(|s| s * g).collect::<Vec<f32>>()
    };
    let a = listen(&mut fx, &quiet(48_000 * 5, 41));
    let s = suggest(&a, Profile::Keys);
    // Une source discrète est remontée, sans dépasser −1 dBFS en crête.
    assert!(s.get(P_OUTPUT) > 3.0, "{}", s.get(P_OUTPUT));
    apply(&mut fx, &s);
    let x = quiet(48_000 * 2, 42);
    let (l, _) = run_mono(&mut fx, &x);
    assert!(peak(&l[9_600..]) < 1.0);
    let level = gain_to_db(rms(&l[9_600..]));
    assert!((level + 18.0).abs() < 4.0, "{level}");
}

#[test]
fn chain_response_matches_the_filters() {
    let mut fx = MixAssistant::default();
    fx.set_param(P_BAND + 3, 1_000.0);
    fx.set_param(P_BAND + 4, -9.0);
    let get = |id| fx.get_param(id);
    assert!((chain_response_db(&get, 1.0, 1_000.0, SR) + 9.0).abs() < 0.05);
    assert!((chain_response_db(&get, 0.5, 1_000.0, SR) + 4.5).abs() < 0.05);
    assert_eq!(chain_response_db(&get, 0.0, 1_000.0, SR), 0.0);
}

