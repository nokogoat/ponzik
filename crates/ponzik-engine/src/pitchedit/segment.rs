//! A pitch track cut into notes.
//!
//! Each voiced run is one or more notes. Within a run, a Viterbi path over
//! whole semitones follows the track smoothed over 100 ms (a vibrato of ±50
//! cents never flips it; a change of note has to hold to pay for the switch).
//! Notes too short to be sung are merged into the neighbour nearest in pitch,
//! each boundary is moved back to where the glide from the previous note
//! starts, and a note sung twice on one pitch (a dip of the level, "la-a")
//! is cut at the dip.

use super::analyze::runs;
use super::{Analysis, DRIFT_WINDOW, HOP, Note, smooth_voiced};

/// Cost of changing note on the Viterbi path (semitones², summed over frames).
const SWITCH: f32 = 5.0;
/// Shortest note kept apart from its neighbours, seconds.
const MIN_NOTE: f64 = 0.1;
/// Depth of the level dip that splits a note on one pitch, dB.
const DIP_DB: f32 = 8.0;

/// Notes of an analysis.
pub(super) fn segment(a: &Analysis) -> Vec<Note> {
    let mut notes = Vec::new();
    for (ra, rb) in runs(&a.pitch) {
        let mut parts = viterbi(&a.pitch[ra..rb]);
        merge_short(&a.pitch[ra..rb], &mut parts);
        refine(&a.pitch[ra..rb], &mut parts);
        for (s, e) in parts {
            for (s, e) in dips(&a.level[ra..rb], s, e) {
                let (fa, fb) = (ra + s, ra + e);
                if let Some((centre, vibrato, level)) = note_stats(a, fa, fb) {
                    notes.push(Note { start: fa as f64 * HOP, end: fb as f64 * HOP, centre, vibrato, level });
                }
            }
        }
    }
    notes
}

/// Segments `(start, end)` of a voiced run where the nearest semitone holds.
fn viterbi(p: &[f32]) -> Vec<(usize, usize)> {
    let n = p.len();
    if n == 0 {
        return Vec::new();
    }
    let s = smooth_voiced(p, (0.05 / HOP).round() as usize);
    let lo = s.iter().copied().fold(f32::MAX, f32::min).floor() as i32 - 1;
    let hi = s.iter().copied().fold(f32::MIN, f32::max).ceil() as i32 + 1;
    let states = (hi - lo + 1).max(1) as usize;
    let cost = |k: usize, j: usize| (s[k] - (lo + j as i32) as f32).powi(2).min(4.0);
    let mut acc: Vec<f32> = (0..states).map(|j| cost(0, j)).collect();
    let mut back = vec![0u16; n * states];
    let mut next = vec![0.0f32; states];
    for k in 1..n {
        let (best_j, best) = acc.iter().enumerate().fold((0, f32::MAX), |m, (j, &v)| if v < m.1 { (j, v) } else { m });
        for j in 0..states {
            let (from, base) = if acc[j] <= best + SWITCH { (j, acc[j]) } else { (best_j, best + SWITCH) };
            next[j] = base + cost(k, j);
            back[k * states + j] = from as u16;
        }
        std::mem::swap(&mut acc, &mut next);
    }
    let mut j = acc.iter().enumerate().fold((0, f32::MAX), |m, (j, &v)| if v < m.1 { (j, v) } else { m }).0;
    let mut path = vec![0usize; n];
    for k in (0..n).rev() {
        path[k] = j;
        j = back[k * states + j] as usize;
    }
    let mut out = Vec::new();
    let mut start = 0;
    for k in 1..=n {
        if k == n || path[k] != path[k - 1] {
            out.push((start, k));
            start = k;
        }
    }
    out
}

fn median_of(p: &[f32]) -> f32 {
    let mut v: Vec<f32> = p.iter().copied().filter(|x| x.is_finite()).collect();
    if v.is_empty() {
        return f32::NAN;
    }
    v.sort_by(f32::total_cmp);
    v[v.len() / 2]
}

/// Segments shorter than [`MIN_NOTE`] join the neighbour nearest in pitch.
fn merge_short(p: &[f32], parts: &mut Vec<(usize, usize)>) {
    let min = (MIN_NOTE / HOP).round() as usize;
    while parts.len() > 1 {
        let Some(i) = (0..parts.len()).filter(|&i| parts[i].1 - parts[i].0 < min).min_by_key(|&i| parts[i].1 - parts[i].0) else { break };
        let m = median_of(&p[parts[i].0..parts[i].1]);
        let dist = |j: usize| (median_of(&p[parts[j].0..parts[j].1]) - m).abs();
        let into = match (i.checked_sub(1), (i + 1 < parts.len()).then_some(i + 1)) {
            (Some(a), Some(b)) => {
                if dist(a) <= dist(b) {
                    a
                } else {
                    b
                }
            }
            (Some(a), None) => a,
            (None, Some(b)) => b,
            (None, None) => break,
        };
        let (lo, hi) = (i.min(into), i.max(into));
        parts[lo] = (parts[lo].0, parts[hi].1);
        parts.remove(hi);
    }
}

/// Each boundary moved back to the start of the glide into the next note.
fn refine(p: &[f32], parts: &mut [(usize, usize)]) {
    let q = smooth_voiced(p, 5);
    for i in 1..parts.len() {
        let (prev, next) = (parts[i - 1], parts[i]);
        let mid_of = |(a, b): (usize, usize)| {
            let trim = ((b - a) / 4).min(20);
            median_of(&p[a + trim..b - trim])
        };
        let (a, b) = (mid_of(prev), mid_of(next));
        if !(a.is_finite() && b.is_finite()) || (b - a).abs() < 0.5 {
            continue;
        }
        let k0 = next.0;
        let lo = prev.0 + 8;
        let hi = next.1.saturating_sub(8);
        if lo + 2 >= hi {
            continue;
        }
        let mid = 0.5 * (a + b);
        let reach = (0.08 / HOP) as usize;
        let crossing = (k0.saturating_sub(reach).max(lo)..(k0 + reach).min(hi - 1))
            .filter(|&k| q[k].is_finite() && q[k + 1].is_finite() && (q[k] - mid).signum() != (q[k + 1] - mid).signum())
            .min_by_key(|&k| k.abs_diff(k0));
        let Some(kc) = crossing else { continue };
        let dir = (b - a).signum();
        let peak = (dir * (q[(kc + 1).min(hi)] - q[kc.saturating_sub(1).max(lo)]) * 0.5).max(1e-4);
        let mut k = kc;
        let limit = kc.saturating_sub((0.06 / HOP) as usize).max(lo);
        while k > limit {
            let slope = dir * (q[k] - q[k - 1]);
            if slope < 0.25 * peak || dir * (q[k - 1] - a) < 0.12 * (b - a).abs() {
                break;
            }
            k -= 1;
        }
        let k = k.clamp(prev.0 + 4, next.1 - 4);
        parts[i - 1].1 = k;
        parts[i].0 = k;
    }
}

/// `[s, e)` cut at the dips of the level (a repeated note on one pitch).
fn dips(level: &[f32], s: usize, e: usize) -> Vec<(usize, usize)> {
    let edge = (0.04 / HOP) as usize;
    let side = (0.15 / HOP) as usize;
    let gap = (0.08 / HOP) as usize;
    if e - s < 2 * edge + 2 {
        return vec![(s, e)];
    }
    let l = smooth_voiced(&level[s..e], 5);
    let mut cuts = Vec::new();
    let mut k = edge;
    while k + edge < l.len() {
        let around = &l[k.saturating_sub(10)..(k + 11).min(l.len())];
        let is_min = around.iter().all(|&v| v >= l[k]);
        let before = l[k.saturating_sub(side)..k].iter().copied().fold(f32::MIN, f32::max);
        let after = l[k + 1..(k + side).min(l.len())].iter().copied().fold(f32::MIN, f32::max);
        if is_min && before - l[k] >= DIP_DB && after - l[k] >= DIP_DB && cuts.last().is_none_or(|&c: &usize| k - c >= gap) {
            cuts.push(k);
            k += gap;
            continue;
        }
        k += 1;
    }
    let mut out = Vec::new();
    let mut a = s;
    for c in cuts {
        out.push((a, s + c));
        a = s + c;
    }
    out.push((a, e));
    out
}

/// Centre (MIDI), vibrato (cents) and peak level (dB) of frames `[fa, fb)`.
pub(super) fn note_stats(a: &Analysis, fa: usize, fb: usize) -> Option<(f32, f32, f32)> {
    let p = a.pitch.get(fa..fb)?;
    let voiced = p.iter().filter(|v| v.is_finite()).count();
    if voiced == 0 {
        return None;
    }
    // The attack and release (glides in and out) say little about the centre:
    // a quarter of the note on each side, 60 ms at most, is left out.
    let trim = (p.len() / 4).min((0.06 / HOP) as usize);
    let held = &p[trim..p.len() - trim];
    let slow = smooth_voiced(held, (DRIFT_WINDOW / HOP / 2.0).round() as usize);
    let (mut sum, mut cnt, mut vib) = (0.0f64, 0u32, 0.0f64);
    for (v, s) in held.iter().zip(&slow) {
        if v.is_finite() && s.is_finite() {
            sum += f64::from(*s);
            vib += f64::from(v - s).powi(2);
            cnt += 1;
        }
    }
    if cnt == 0 {
        return None;
    }
    let centre = (sum / f64::from(cnt)) as f32;
    let vibrato = ((vib / f64::from(cnt)).sqrt() * std::f64::consts::SQRT_2 * 100.0) as f32;
    let level = a.level.get(fa..fb).map_or(-120.0, |l| l.iter().copied().fold(-120.0f32, f32::max));
    Some((centre, vibrato, level))
}
