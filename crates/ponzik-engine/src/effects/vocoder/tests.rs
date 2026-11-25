use ponzik_dsp::biquad::{Biquad, BiquadCoefs};

use super::*;
use crate::effects::testing::*;

/// Runs the vocoder by blocks of 256 on `carrier` (the track) with `modulator` as the key.
fn vocode(v: &mut Vocoder, carrier: &[f32], modulator: Option<&[f32]>) -> (Vec<f32>, Vec<f32>) {
    let (mut l, mut r) = (carrier.to_vec(), carrier.to_vec());
    for (k, (a, b)) in l.chunks_mut(256).zip(r.chunks_mut(256)).enumerate() {
        if let Some(m) = modulator {
            let key = &m[k * 256..k * 256 + a.len()];
            v.sidechain(key, key);
        }
        v.process(a, b);
    }
    (l, r)
}

/// RMS of `x` through an octave-wide band-pass at `hz`.
fn band_rms(x: &[f32], hz: f32) -> f32 {
    let c = BiquadCoefs::bandpass(hz, 1.41, SR);
    let (mut a, mut b) = (Biquad::new(c), Biquad::new(c));
    let y: Vec<f32> = x.iter().map(|&s| b.process(a.process(s))).collect();
    rms(&y)
}

fn vocoder() -> Vocoder {
    let mut v = Vocoder::default();
    v.set_sample_rate(SR);
    v
}

#[test]
fn layout_is_log_spaced_and_formant_shifts_the_carrier() {
    let l = Layout::new(16, 100.0, 6_400.0, 1.0, 0.0, SR);
    assert_eq!(l.count, 16);
    assert!((l.modulator[0] - 100.0).abs() < 0.01 && (l.modulator[15] - 6_400.0).abs() < 0.5);
    let r = l.modulator[1] / l.modulator[0];
    assert!((l.modulator[9] / l.modulator[8] - r).abs() < 1e-3);
    assert_eq!(l.modulator, l.carrier);
    let up = Layout::new(16, 100.0, 6_400.0, 1.0, 12.0, SR);
    assert!((up.carrier[3] / up.modulator[3] - 2.0).abs() < 1e-3);
    // Wider bands, lower Q; the range is kept sane.
    assert!(Layout::new(16, 100.0, 6_400.0, 2.0, 0.0, SR).q < l.q);
    let odd = Layout::new(8, 900.0, 900.0, 1.0, 0.0, SR);
    assert!(odd.modulator[7] >= 1_800.0 - 1.0);
}

#[test]
fn a_burst_in_one_band_raises_that_band_only() {
    let mut v = vocoder();
    v.set_param(P_BANDS, 6.0); // 24 bands
    v.set_param(P_UNVOICED, 0.0);
    v.set_param(P_SPREAD, 0.0);
    let layout = v.layout();
    let (near, far) = (layout.modulator[8], layout.modulator[20]);
    let frames = 48_000;
    let carrier = noise(0.25, frames, 3);
    // Silence, then half a second of a sine at the center of band 8.
    let mut modulator = vec![0.0; frames];
    let burst = sine(near, 0.5, frames / 2);
    modulator[frames / 2..].copy_from_slice(&burst);
    let (l, r) = vocode(&mut v, &carrier, Some(&modulator));
    let out: Vec<f32> = l.iter().zip(&r).map(|(a, b)| 0.5 * (a + b)).collect();
    let (before, after) = (&out[4_800..frames / 2], &out[frames / 2 + 4_800..]);
    let near_after = band_rms(after, near);
    let far_after = band_rms(after, far);
    assert!(near_after > 0.02, "band output {near_after}");
    assert!(near_after > 30.0 * band_rms(before, near).max(1e-6), "{near_after} vs silence");
    assert!(near_after > 10.0 * far_after, "near {near_after} far {far_after}");
    // The interface sees it in that band.
    let s = v.shared_state();
    assert_eq!(s.bands(), 24);
    assert!(s.modulator(8) > 10.0 * s.modulator(20) && s.output(8) > 10.0 * s.output(20));
    assert!(s.keyed());
}

#[test]
fn formant_shift_moves_the_output_band() {
    let mut v = vocoder();
    v.set_param(P_UNVOICED, 0.0);
    v.set_param(P_FORMANT, 12.0);
    let hz = v.layout().modulator[6];
    let frames = 48_000;
    let (l, _) = vocode(&mut v, &noise(0.25, frames, 9), Some(&sine(hz, 0.5, frames)));
    let tail = &l[12_000..];
    assert!(band_rms(tail, 2.0 * hz) > 3.0 * band_rms(tail, hz), "{} {}", band_rms(tail, 2.0 * hz), band_rms(tail, hz));
}

#[test]
fn unvoiced_noise_passes_when_enabled() {
    // An internal chord following channel 1, which holds nothing: the carrier is silent.
    let run = |amount: f32| {
        let mut v = vocoder();
        v.set_param(P_CARRIER, 1.0);
        v.set_param(P_MIDI_CHANNEL, 1.0);
        v.set_param(P_UNVOICED, amount);
        v.channel_notes(&[ChannelNotes::EMPTY]);
        let hiss = noise(0.3, 24_000, 5);
        let (l, _) = vocode(&mut v, &vec![0.0; 24_000], Some(&hiss));
        (rms(&l[4_800..]), v.shared_state().unvoiced())
    };
    let (on, unvoiced) = run(1.0);
    let (off, _) = run(0.0);
    assert!(unvoiced > 0.8, "detector {unvoiced}");
    assert!(on > 0.01, "noise through: {on}");
    assert!(off < 1e-5, "nothing without it: {off}");
}

#[test]
fn a_voiced_tone_is_not_taken_for_a_consonant() {
    let mut v = vocoder();
    let (_, _) = vocode(&mut v, &noise(0.2, 24_000, 2), Some(&sine(220.0, 0.5, 24_000)));
    assert!(v.shared_state().unvoiced() < 0.05);
}

#[test]
fn the_internal_chord_follows_the_channel_and_the_drone() {
    // Drone without a channel: a pulse at C3 shaped by the track itself.
    let mut v = vocoder();
    v.set_param(P_CARRIER, 2.0);
    let (l, _) = vocode(&mut v, &noise(0.3, 24_000, 4), None);
    assert!(rms(&l[4_800..]) > 0.01);
    assert!(!v.shared_state().keyed());
    // With a channel: silent until a note is held there.
    let mut v = vocoder();
    v.set_param(P_CARRIER, 1.0);
    v.set_param(P_MIDI_CHANNEL, 2.0);
    v.set_param(P_UNVOICED, 0.0);
    let voice = sine(300.0, 0.4, 24_000);
    let mut held = [ChannelNotes::EMPTY; 3];
    v.channel_notes(&held);
    let (quiet, _) = vocode(&mut v, &vec![0.0; 24_000], Some(&voice));
    assert!(rms(&quiet) < 1e-5);
    held[1].note_on(60);
    v.channel_notes(&held);
    let (loud, _) = vocode(&mut v, &vec![0.0; 24_000], Some(&voice));
    assert!(rms(&loud[4_800..]) > 0.01, "{}", rms(&loud[4_800..]));
}

#[test]
fn dry_mix_passes_the_track_untouched() {
    let mut v = vocoder();
    v.set_param(P_MIX, 0.0);
    let x = sine(440.0, 0.3, 48_000);
    let (l, _) = vocode(&mut v, &x, Some(&noise(0.5, 48_000, 1)));
    // Once the mix has glided there (20 ms smoothing).
    assert!(l[24_000..].iter().zip(&x[24_000..]).all(|(a, b)| (a - b).abs() < 1e-5));
}

#[test]
fn spread_sends_neighbour_bands_to_both_sides() {
    let balance = |spread: f32| {
        let mut v = vocoder();
        v.set_param(P_SPREAD, spread);
        v.set_param(P_UNVOICED, 0.0);
        let hz = v.layout().modulator[4];
        let (l, r) = vocode(&mut v, &noise(0.25, 24_000, 8), Some(&sine(hz, 0.5, 24_000)));
        rms(&l[9_600..]) / rms(&r[9_600..])
    };
    // Band 4 is even: on the left (its neighbours, on the right, overlap it a little).
    assert!((balance(0.0) - 1.0).abs() < 0.05, "{}", balance(0.0));
    assert!(balance(1.0) > 1.6, "{}", balance(1.0));
}

#[test]
fn level_is_sane_for_a_voice_on_a_saw() {
    let mut v = vocoder();
    v.set_param(P_CARRIER, 1.0);
    let voice = sine(180.0, 0.3, 48_000);
    let (l, _) = vocode(&mut v, &vec![0.0; 48_000], Some(&voice));
    let level = rms(&l[12_000..]);
    assert!(level > 0.03 && level < 1.0, "{level}");
}

#[test]
fn stable() {
    assert_stable_at_extremes(|| Box::new(Vocoder::default()), 100.0);
}

#[test]
fn stable_with_a_loud_key_and_odd_input() {
    let mut v = vocoder();
    v.set_param(P_ATTACK, 0.000_5);
    v.set_param(P_RELEASE, 2.0);
    v.set_param(P_BANDS, 10.0);
    v.set_param(P_WIDTH, 4.0);
    let mut x = noise(1.0, 9_600, 3);
    x[100] = f32::NAN;
    x[200] = f32::INFINITY;
    let key = noise(1.0, 9_600, 6);
    let (l, r) = vocode(&mut v, &x, Some(&key));
    assert!(l.iter().chain(&r).all(|s| s.is_finite() && s.abs() < 100.0));
}
