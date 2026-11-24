//! The pitch track of a whole file: the Pitch Correction's detector run in
//! parallel over chunks of the file (each with half a second of run-up), a
//! first pass over the widest range to find where the voice lies, a second
//! one over the voice type that fits it (fewer octave errors), then
//! resampled to a 2 ms grid and cleaned (octave jumps folded back, spikes
//! removed, crumbs of voicing dropped, tiny gaps bridged).

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use super::{Analysis, HOP, segment};
use crate::effects::autotune::{Detector, RANGE_NOTES, hz_to_midi};

/// The widest input range of the detector ("Instrument").
const WIDE: usize = 5;
/// Run-up of each chunk, seconds.
const RUN_UP: f32 = 0.5;
/// Voiced runs shorter than this are dropped, seconds.
const MIN_RUN: f64 = 0.04;
/// Unvoiced gaps up to this long inside a sung line are bridged, seconds.
const BRIDGE: f64 = 0.008;

/// Progress (0 … 1000) and a way to stop early, both optional.
#[derive(Clone, Copy, Default)]
pub struct Watch<'a> {
    pub progress: Option<&'a AtomicU32>,
    pub cancel: Option<&'a AtomicBool>,
}

impl Watch<'_> {
    fn set(&self, permille: u32) {
        if let Some(p) = self.progress {
            p.store(permille.min(1000), Ordering::Relaxed);
        }
    }

    fn cancelled(&self) -> bool {
        self.cancel.is_some_and(|c| c.load(Ordering::Relaxed))
    }
}

/// Analyses a file (channels mixed to mono).
pub fn analyze(left: &[f32], right: &[f32], sample_rate: f32) -> Analysis {
    analyze_with(left, right, sample_rate, None, None).unwrap_or_default()
}

/// [`analyze`], reporting progress (‰) and stopping (`None`) when `cancel` is set.
pub fn analyze_with(
    left: &[f32],
    right: &[f32],
    sample_rate: f32,
    progress: Option<&AtomicU32>,
    cancel: Option<&AtomicBool>,
) -> Option<Analysis> {
    let watch = Watch { progress, cancel };
    let sr = sample_rate.max(8_000.0);
    let mono: Vec<f32> = if right.len() == left.len() {
        left.iter().zip(right).map(|(l, r)| 0.5 * (l + r)).collect()
    } else {
        left.to_vec()
    };
    let raw = track(&mono, sr, WIDE, watch, 0, 450)?;
    let range = fitting_range(&raw);
    let raw = if range == WIDE { raw } else { track(&mono, sr, range, watch, 450, 900)? };
    let frames = ((mono.len() as f64 / f64::from(sr)) / HOP).ceil() as usize;
    let mut pitch = to_grid(&raw, frames, sr);
    clean(&mut pitch);
    let level = levels(&mono, sr, frames);
    let mut a = Analysis { sample_rate: sr, len: mono.len(), pitch, level, notes: Vec::new() };
    if watch.cancelled() {
        return None;
    }
    a.notes = segment::segment(&a);
    watch.set(1000);
    Some(a)
}

/// A raw detector frame: time (samples) and pitch (MIDI, NaN unvoiced).
type Raw = (f64, f32);

/// Detector frames over the whole signal, chunks in parallel.
fn track(x: &[f32], sr: f32, range: usize, watch: Watch, from: u32, to: u32) -> Option<Vec<Raw>> {
    let threads = std::thread::available_parallelism().map_or(2, |n| n.get()).clamp(1, 16);
    let min_chunk = (8.0 * sr) as usize;
    let chunk = x.len().div_ceil(threads).max(min_chunk);
    let chunks: Vec<(usize, usize)> = (0..x.len().div_ceil(chunk.max(1))).map(|k| (k * chunk, ((k + 1) * chunk).min(x.len()))).collect();
    let done = AtomicU32::new(0);
    let total = x.len().max(1) as f64;
    let parts: Vec<Option<Vec<Raw>>> = std::thread::scope(|s| {
        let handles: Vec<_> = chunks
            .iter()
            .map(|&(a, b)| {
                let done = &done;
                s.spawn(move || {
                    let run_up = (RUN_UP * sr) as usize;
                    let start = a.saturating_sub(run_up);
                    let mut det = Detector::new(sr);
                    det.configure(range, 0.5, true);
                    let mut out = Vec::with_capacity((b - a) / det.hop_samples().max(1) + 4);
                    for (k, &v) in x[start..b].iter().enumerate() {
                        if let Some(f) = det.push(v) {
                            let t = f.time + start as f64;
                            if t >= a as f64 && t < b as f64 {
                                out.push((t, if f.voiced && f.hz > 0.0 { hz_to_midi(f.hz, 440.0) } else { f32::NAN }));
                            }
                        }
                        if k % 16_384 == 16_383 {
                            let n = done.fetch_add(16_384, Ordering::Relaxed) + 16_384;
                            watch.set(from + ((to - from) as f64 * f64::from(n) / total) as u32);
                            if watch.cancelled() {
                                return None;
                            }
                        }
                    }
                    Some(out)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().ok().flatten()).collect()
    });
    let mut all = Vec::new();
    for p in parts {
        all.extend(p?);
    }
    watch.set(to);
    Some(all)
}

/// The voice type whose range holds the sung pitches best (the widest when none does).
fn fitting_range(raw: &[Raw]) -> usize {
    let mut v: Vec<f32> = raw.iter().map(|r| r.1).filter(|p| p.is_finite()).collect();
    if v.len() < 50 {
        return WIDE;
    }
    v.sort_by(f32::total_cmp);
    let lo = v[v.len() * 3 / 100];
    let hi = v[v.len() * 97 / 100];
    let mid = v[v.len() / 2];
    (0..WIDE)
        .filter(|&r| f32::from(RANGE_NOTES[r].0) <= lo - 0.5 && f32::from(RANGE_NOTES[r].1) >= hi + 0.5)
        .min_by(|&a, &b| {
            let off = |r: usize| (0.5 * (f32::from(RANGE_NOTES[r].0) + f32::from(RANGE_NOTES[r].1)) - mid).abs();
            off(a).total_cmp(&off(b))
        })
        .unwrap_or(WIDE)
}

/// Raw frames resampled at `k * HOP`: voiced where the nearest frame is,
/// the pitch linear between voiced neighbours.
fn to_grid(raw: &[Raw], frames: usize, sr: f32) -> Vec<f32> {
    let mut out = vec![f32::NAN; frames];
    let mut j = 0usize;
    for (k, o) in out.iter_mut().enumerate() {
        let t = k as f64 * HOP * f64::from(sr);
        while j + 1 < raw.len() && raw[j + 1].0 <= t {
            j += 1;
        }
        let Some(&(ta, pa)) = raw.get(j) else { break };
        let (tb, pb) = raw.get(j + 1).copied().unwrap_or((ta, pa));
        if t < ta {
            *o = if ta - t < 0.004 * f64::from(sr) { pa } else { f32::NAN };
            continue;
        }
        let nearest = if (t - ta) <= (tb - t) { pa } else { pb };
        if !nearest.is_finite() {
            continue;
        }
        *o = if pa.is_finite() && pb.is_finite() && tb > ta {
            pa + (pb - pa) * ((t - ta) / (tb - ta)) as f32
        } else {
            nearest
        };
    }
    out
}

/// Runs of finite values: `(start, end)` frame indices.
pub(super) fn runs(v: &[f32]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, p) in v.iter().enumerate() {
        match (p.is_finite(), start) {
            (true, None) => start = Some(i),
            (false, Some(a)) => {
                out.push((a, i));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(a) = start {
        out.push((a, v.len()));
    }
    out
}

fn median(v: &mut [f32]) -> f32 {
    v.sort_by(f32::total_cmp);
    v[v.len() / 2]
}

/// Octave jumps folded back, spikes removed, crumbs dropped, gaps bridged.
fn clean(p: &mut [f32]) {
    let n = p.len();
    // Octave errors: a frame an octave off the median around it, first over
    // ±200 ms (an error that lasts a while), then over ±50 ms.
    let mut buf = Vec::with_capacity(256);
    for half in [100usize, 25] {
        let snapshot = p.to_vec();
        for i in 0..n {
            if !snapshot[i].is_finite() {
                continue;
            }
            buf.clear();
            buf.extend(snapshot[i.saturating_sub(half)..(i + half + 1).min(n)].iter().copied().filter(|v| v.is_finite()));
            if buf.len() < 5 {
                continue;
            }
            let m = median(&mut buf);
            let d = snapshot[i] - m;
            if d.abs() > 9.0 {
                let octaves = (d / 12.0).round();
                if octaves != 0.0 && (d - 12.0 * octaves).abs() < 3.0 {
                    p[i] = snapshot[i] - 12.0 * octaves;
                }
            }
        }
    }
    // Spikes: median of 5 voiced neighbours.
    let snapshot = p.to_vec();
    for i in 0..n {
        if !snapshot[i].is_finite() {
            continue;
        }
        buf.clear();
        buf.extend(snapshot[i.saturating_sub(2)..(i + 3).min(n)].iter().copied().filter(|v| v.is_finite()));
        if buf.len() >= 3 {
            p[i] = median(&mut buf);
        }
    }
    // Tiny gaps inside a line are bridged.
    let bridge = (BRIDGE / HOP).round() as usize;
    let r = runs(p);
    for w in r.windows(2) {
        let (a_end, b_start) = (w[0].1, w[1].0);
        let (pa, pb) = (p[a_end - 1], p[b_start]);
        if b_start - a_end <= bridge && (pa - pb).abs() < 1.0 {
            let len = (b_start - a_end + 1) as f32;
            for (j, v) in p[a_end..b_start].iter_mut().enumerate() {
                *v = pa + (pb - pa) * (j + 1) as f32 / len;
            }
        }
    }
    // Crumbs.
    let min = (MIN_RUN / HOP).round() as usize;
    for (a, b) in runs(p) {
        if b - a < min {
            p[a..b].fill(f32::NAN);
        }
    }
}

/// RMS level (dBFS) over 10 ms around each frame.
fn levels(x: &[f32], sr: f32, frames: usize) -> Vec<f32> {
    let half = (0.005 * sr) as usize;
    let mut sum = vec![0.0f64; x.len() + 1];
    for (i, v) in x.iter().enumerate() {
        sum[i + 1] = sum[i] + f64::from(*v) * f64::from(*v);
    }
    (0..frames)
        .map(|k| {
            let c = (k as f64 * HOP * f64::from(sr)) as usize;
            let a = c.saturating_sub(half).min(x.len());
            let b = (c + half).min(x.len());
            if b <= a {
                return -120.0;
            }
            let ms = (sum[b] - sum[a]) / (b - a) as f64;
            (10.0 * (ms + 1e-12).log10()).max(-120.0) as f32
        })
        .collect()
}
