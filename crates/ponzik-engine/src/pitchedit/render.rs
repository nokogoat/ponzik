//! Rendering the edits: offline TD-PSOLA with a local time map.
//!
//! Analysis marks are laid over the whole file: one per period where the
//! voice is pitched (the first of a run on the loudest point of its period,
//! each next one aligned on the previous by cross-correlation, as the real-time
//! shifter does), every 5 ms elsewhere. Output marks walk the edited region at
//! the output period (the input period over the pitch ratio); each takes the
//! analysis mark nearest the point of the file the time map says plays there,
//! and lays its grain: the input around the mark under a two-sided Hann window
//! whose halves are the spacings to the neighbouring output marks (no longer
//! than the input's), so the windows always add up to one. A grain read
//! faster or slower moves the formants; read as is, it keeps them. With no
//! change at all the grains rebuild the input sample for sample.
//!
//! Only the regions that differ are synthesized: each spans the notes edited
//! (and the stretch of time between moved boundaries) with 100 ms to spare,
//! and is crossfaded over 10 ms into the untouched input, where both are the
//! same. A render made from a previous one redoes only the regions whose
//! edits changed. Each edited note keeps the level it had (measured before
//! and after), then gets the gain asked for.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use ponzik_dsp::util::midi_to_hz;

use super::{Analysis, HOP, NoteEdit, interp};
use crate::Sample;

/// Extra time around an edited region, seconds.
const PAD: f64 = 0.1;
/// Crossfade into the input at a region's edges, seconds.
const XFADE: f64 = 0.01;
/// Mark spacing where nothing is pitched, seconds.
const UNVOICED_STEP: f64 = 0.005;
/// Pitch, formant and level glide from a note into the next one it touches, seconds.
const RAMP: f64 = 0.06;
/// Drift and vibrato edits fade in and out at a note's edges, seconds.
const TAPER: f64 = 0.02;

/// Where each moment of the output comes from in the file: piecewise linear
/// through the notes' boundaries (old place → new place).
#[derive(Clone, Debug, PartialEq)]
pub struct TimeMap {
    /// `(output, input)` seconds, both increasing.
    points: Vec<(f64, f64)>,
}

impl TimeMap {
    pub fn new(edits: &[NoteEdit], duration: f64) -> TimeMap {
        let mut pts: Vec<(f64, f64)> = vec![(0.0, 0.0)];
        let mut sorted: Vec<&NoteEdit> = edits.iter().collect();
        sorted.sort_by(|a, b| a.start.total_cmp(&b.start));
        for e in sorted {
            pts.push((e.new_start, e.start));
            pts.push((e.new_end, e.end));
        }
        pts.push((duration, duration));
        pts.sort_by(|a, b| a.1.total_cmp(&b.1));
        let mut points: Vec<(f64, f64)> = Vec::with_capacity(pts.len());
        for (d, s) in pts {
            match points.last_mut() {
                Some(last) if s - last.1 < 1e-9 => last.0 = last.0.max(d),
                Some(last) => {
                    let d = d.max(last.0 + 1e-6);
                    points.push((d, s));
                }
                None => points.push((d, s)),
            }
        }
        TimeMap { points }
    }

    pub fn is_identity(&self) -> bool {
        self.points.iter().all(|(d, s)| (d - s).abs() < 1e-7)
    }

    fn lookup(points: &[(f64, f64)], x: f64, from: fn(&(f64, f64)) -> f64, to: fn(&(f64, f64)) -> f64) -> f64 {
        let i = points.partition_point(|p| from(p) <= x);
        if i == 0 || i >= points.len() {
            // Outside: time runs as in the file.
            let p = if i == 0 { points.first() } else { points.last() };
            return p.map_or(x, |p| to(p) + (x - from(p)));
        }
        let (a, b) = (&points[i - 1], &points[i]);
        let span = from(b) - from(a);
        if span <= 1e-12 {
            return to(a);
        }
        to(a) + (to(b) - to(a)) * (x - from(a)) / span
    }

    /// Point of the file heard at output time `t`.
    pub fn src(&self, t: f64) -> f64 {
        Self::lookup(&self.points, t, |p| p.0, |p| p.1)
    }

    /// Output time where the file's point `s` is heard.
    pub fn dst(&self, s: f64) -> f64 {
        Self::lookup(&self.points, s, |p| p.1, |p| p.0)
    }

    /// Output spans `(from, to)` where the map is not the identity.
    fn moved(&self) -> Vec<(f64, f64)> {
        self.points
            .windows(2)
            .filter(|w| (w[0].0 - w[0].1).abs() > 1e-7 || (w[1].0 - w[1].1).abs() > 1e-7)
            .map(|w| (w[0].0.min(w[0].1), w[1].0.max(w[1].1)))
            .collect()
    }
}

/// Pitch shift (semitones), formant shift (semitones) and level change (dB)
/// at each frame of the file.
struct Curves {
    shift: Vec<f32>,
    formant: Vec<f32>,
}

fn smoothstep(x: f64) -> f32 {
    let x = x.clamp(0.0, 1.0);
    (x * x * (3.0 - 2.0 * x)) as f32
}

fn curves(a: &Analysis, edits: &[NoteEdit]) -> Curves {
    let n = a.pitch.len();
    let mut shift = vec![0.0f32; n];
    let mut formant = vec![0.0f32; n];
    let mut sorted: Vec<&NoteEdit> = edits.iter().collect();
    sorted.sort_by(|x, y| x.start.total_cmp(&y.start));
    let taper = TAPER / HOP;
    let mut prev: Option<(usize, f32, f32)> = None;
    for e in sorted {
        let c = a.note_curves(e.start, e.end, e.centre);
        let (fa, fb) = (c.first, c.first + c.pitch.len());
        if fb <= fa {
            continue;
        }
        for (j, k) in (fa..fb).enumerate() {
            let w = smoothstep(j.min(fb - 1 - k) as f64 / taper);
            shift[k] = e.shift() - e.drift * c.drift[j] * w + (e.vibrato - 1.0) * c.vibrato[j] * w;
            formant[k] = e.formant;
        }
        // Touching the previous note: glide from where it ends.
        if let Some((pend, ps, pf)) = prev
            && pend + 1 >= fa
        {
            let r = ((RAMP / HOP) as usize).min((fb - fa) * 3 / 10).max(1);
            for j in 0..r.min(fb - fa) {
                let k = fa + j;
                let w = smoothstep(j as f64 / r as f64);
                shift[k] = ps + (shift[k] - ps) * w;
                formant[k] = pf + (formant[k] - pf) * w;
            }
        }
        prev = Some((fb, shift[fb - 1], formant[fb - 1]));
    }
    Curves { shift, formant }
}

#[derive(Clone, Copy, Debug)]
struct Mark {
    pos: i64,
    voiced: bool,
}

#[inline]
fn at(x: &[f32], i: i64) -> f32 {
    if i < 0 { 0.0 } else { x.get(i as usize).copied().unwrap_or(0.0) }
}

/// Catmull-Rom interpolation of `x` at `t`.
#[inline]
fn cubic(x: &[f32], t: f64) -> f32 {
    let i = t.floor() as i64;
    let f = (t - i as f64) as f32;
    if f == 0.0 {
        return at(x, i);
    }
    let (p0, p1, p2, p3) = (at(x, i - 1), at(x, i), at(x, i + 1), at(x, i + 2));
    p1 + 0.5 * f * (p2 - p0 + f * (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3 + f * (3.0 * (p1 - p2) + p3 - p0)))
}

/// Analysis marks over the whole file.
fn marks(x: &[f32], a: &Analysis) -> Vec<Mark> {
    let sr = f64::from(a.sample_rate);
    let len = x.len() as i64;
    let step = (UNVOICED_STEP * sr).round() as i64;
    let mut out: Vec<Mark> = Vec::with_capacity((x.len() as f64 / sr * 250.0) as usize + 16);
    let mut pos: i64 = 0;
    while pos < len {
        let p = a.pitch_at(pos as f64 / sr);
        if !p.is_finite() {
            out.push(Mark { pos, voiced: false });
            pos += step;
            continue;
        }
        let period = (sr / f64::from(midi_to_hz(p))).clamp(16.0, 0.05 * sr);
        let m = match out.last() {
            Some(last) if last.voiced => align(x, last.pos, pos, period),
            _ => {
                // The loudest point of the first period.
                let mut best = (pos, -1.0f32);
                for k in pos..(pos + period as i64).min(len) {
                    let v = at(x, k).abs();
                    if v > best.1 {
                        best = (k, v);
                    }
                }
                best.0
            }
        };
        let m = m.max(out.last().map_or(0, |l| l.pos + 1));
        out.push(Mark { pos: m, voiced: true });
        pos = m + period.round() as i64;
    }
    out
}

/// The point within a quarter period of `c` whose period before best matches the one before `last`.
fn align(x: &[f32], last: i64, c: i64, period: f64) -> i64 {
    let n = (period.round() as i64).clamp(8, 2_048);
    let reach = (period * 0.25).ceil() as i64;
    let score = |d: i64| -> f32 {
        let (mut xy, mut yy) = (0.0f32, 1e-9f32);
        for k in 1..=n {
            let y = at(x, c + d - k);
            xy += y * at(x, last - k);
            yy += y * y;
        }
        xy / yy.sqrt()
    };
    let coarse = ((period / 64.0).round() as i64).max(1);
    let mut best = (0i64, f32::MIN);
    let mut d = -reach;
    while d <= reach {
        let v = score(d);
        if v > best.1 {
            best = (d, v);
        }
        d += coarse;
    }
    if coarse > 1 {
        let centre = best.0;
        for d in (centre - coarse + 1)..(centre + coarse) {
            if d != centre && d.abs() <= reach {
                let v = score(d);
                if v > best.1 {
                    best = (d, v);
                }
            }
        }
    }
    c + best.0
}

/// Output spans that need synthesis for `edits`: edited notes and moved time, padded.
fn spans(edits: &[NoteEdit], map: &TimeMap, duration: f64) -> Vec<(f64, f64)> {
    let mut v: Vec<(f64, f64)> =
        edits.iter().filter(|e| !e.is_identity()).map(|e| (e.start.min(e.new_start), e.end.max(e.new_end))).collect();
    v.extend(map.moved());
    merge(v, duration)
}

fn merge(mut v: Vec<(f64, f64)>, duration: f64) -> Vec<(f64, f64)> {
    v.iter_mut().for_each(|s| *s = ((s.0 - PAD).max(0.0), (s.1 + PAD).min(duration)));
    v.retain(|s| s.1 > s.0);
    v.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut out: Vec<(f64, f64)> = Vec::new();
    for s in v {
        match out.last_mut() {
            Some(last) if s.0 <= last.1 + 2.0 * XFADE => last.1 = last.1.max(s.1),
            _ => out.push(s),
        }
    }
    out
}

/// Edits that touch the output span `r`.
fn touching(edits: &[NoteEdit], r: (f64, f64)) -> Vec<NoteEdit> {
    let mut v: Vec<NoteEdit> =
        edits.iter().filter(|e| e.start.min(e.new_start) < r.1 && e.end.max(e.new_end) > r.0).copied().collect();
    v.sort_by(|a, b| a.start.total_cmp(&b.start));
    v
}

/// What a render has to know about the file.
struct Ctx<'a> {
    src: [&'a [f32]; 2],
    mono: Vec<f32>,
    sr: f64,
    marks: Vec<Mark>,
    curves: Curves,
    map: TimeMap,
}

/// One output grain: where it goes, the analysis mark it takes, how it is read.
#[derive(Clone, Copy)]
struct Grain {
    at: f64,
    mark: usize,
    phi: f64,
}

/// Output grains covering `[a, b)`: the output period is the input's over
/// the pitch ratio; where nothing is shifted and time runs at its own speed,
/// a grain goes back exactly where its mark maps (the walk re-synchronizes
/// with the file after a shifted note, so the rest stays sample-exact).
fn grains(ctx: &Ctx, a: i64, b: i64) -> Vec<Grain> {
    let sr = ctx.sr;
    let marks = &ctx.marks;
    let lead = (0.06 * sr) as i64;
    let first = marks.partition_point(|m| m.pos < a - lead).min(marks.len() - 1);
    let mut ts = marks[first].pos as f64;
    let end = (b + lead) as f64;
    let mut out: Vec<Grain> = Vec::new();
    while ts < end {
        let u = ctx.map.src(ts / sr) * sr;
        let k = marks.partition_point(|m| (m.pos as f64) < u);
        let i = match (k.checked_sub(1), marks.get(k)) {
            (Some(p), Some(n)) if u - marks[p].pos as f64 > n.pos as f64 - u => k,
            (Some(p), _) => p,
            (None, _) => 0,
        };
        let m = marks[i];
        let fall = if i + 1 < marks.len() { (marks[i + 1].pos - m.pos) as f64 } else { UNVOICED_STEP * sr };
        let frame = m.pos as f64 / sr / HOP;
        let (r, phi) = if m.voiced {
            let r = 2f64.powf(f64::from(interp(&ctx.curves.shift, frame)) / 12.0);
            (r.clamp(0.25, 4.0), 2f64.powf(f64::from(interp(&ctx.curves.formant, frame)) / 12.0).clamp(0.25, 4.0))
        } else {
            (1.0, 1.0)
        };
        if (r - 1.0).abs() < 1e-6 && (phi - 1.0).abs() < 1e-6 {
            let t = ts / sr;
            let slope = (ctx.map.src(t + 0.002) - ctx.map.src(t)) / 0.002;
            let snap = ctx.map.dst(m.pos as f64 / sr) * sr;
            if (slope - 1.0).abs() < 1e-6 && out.last().is_none_or(|g| snap > g.at + 1.0) {
                ts = snap;
            }
        }
        out.push(Grain { at: ts, mark: i, phi });
        ts += (fall / r).max(1.0);
    }
    out
}

/// Synthesizes output samples `[a, b)` into `out` (two channels).
fn synth(ctx: &Ctx, a: i64, b: i64, out: &mut [Vec<f32>; 2]) {
    let len = (b - a).max(0) as usize;
    out.iter_mut().for_each(|o| {
        o.clear();
        o.resize(len, 0.0);
    });
    let marks = &ctx.marks;
    if marks.len() < 2 || len == 0 {
        return;
    }
    let step = UNVOICED_STEP * ctx.sr;
    let list = grains(ctx, a, b);
    for (k, g) in list.iter().enumerate() {
        let i = g.mark;
        let rise = if i > 0 { (marks[i].pos - marks[i - 1].pos) as f64 } else { step };
        let fall = if i + 1 < marks.len() { (marks[i + 1].pos - marks[i].pos) as f64 } else { step };
        // Halves: the spacings to the neighbouring grains, at most the input's.
        let rise_out = k.checked_sub(1).map_or(rise / g.phi, |p| (g.at - list[p].at).min(rise / g.phi)).max(1.0);
        let fall_out = list.get(k + 1).map_or(fall / g.phi, |n| (n.at - g.at).min(fall / g.phi)).max(1.0);
        let t0 = (g.at - rise_out).ceil() as i64;
        let t1 = (g.at + fall_out).floor() as i64;
        let pos = marks[i].pos as f64;
        for t in t0.max(a)..=t1.min(b - 1) {
            let d = t as f64 - g.at;
            let half = if d < 0.0 { rise_out } else { fall_out };
            let w = (0.5 + 0.5 * (std::f64::consts::PI * d / half).cos()) as f32;
            let s = pos + d * g.phi;
            let idx = (t - a) as usize;
            out[0][idx] += w * cubic(ctx.src[0], s);
            out[1][idx] += w * cubic(ctx.src[1], s);
        }
    }
}

fn rms(x: &[f32]) -> f64 {
    if x.is_empty() {
        return 0.0;
    }
    (x.iter().map(|v| f64::from(*v) * f64::from(*v)).sum::<f64>() / x.len() as f64).sqrt()
}

/// Level of each edited note kept, then its gain applied: a gain per output
/// sample of `[a, b)`, 20 ms ramps.
fn gains(ctx: &Ctx, edits: &[NoteEdit], a: i64, b: i64, out: &[Vec<f32>; 2]) -> Vec<f32> {
    let sr = ctx.sr;
    let len = (b - a).max(0) as usize;
    let frames = (len as f64 / sr / HOP).ceil() as usize + 2;
    let mut g = vec![1.0f32; frames];
    let mono = |o: &[Vec<f32>; 2], s: usize, e: usize| -> Vec<f32> { (s..e).map(|i| 0.5 * (o[0][i] + o[1][i])).collect() };
    for e in edits.iter().filter(|e| !e.is_identity()) {
        let (s0, s1) = ((e.start * sr) as usize, ((e.end * sr) as usize).min(ctx.mono.len()));
        let (d0, d1) = ((e.new_start * sr) as i64 - a, (e.new_end * sr) as i64 - a);
        if d1 <= 0 || d0 >= len as i64 || s1 <= s0 {
            continue;
        }
        let (d0, d1) = (d0.max(0) as usize, (d1 as usize).min(len));
        let before = rms(&ctx.mono[s0..s1]);
        let after = rms(&mono(out, d0, d1));
        let keep = if after > 1e-9 && before > 1e-9 { (before / after).clamp(0.5, 2.0) } else { 1.0 };
        let total = keep as f32 * ponzik_dsp::util::db_to_gain(e.gain);
        let f0 = (d0 as f64 / sr / HOP).round() as usize;
        let f1 = ((d1 as f64 / sr / HOP).round() as usize).min(frames);
        g[f0.min(frames)..f1].fill(total);
    }
    let g = smooth(&g, 5);
    (0..len).map(|i| interp(&g, i as f64 / sr / HOP)).collect()
}

fn smooth(v: &[f32], half: usize) -> Vec<f32> {
    (0..v.len())
        .map(|i| {
            let s = &v[i.saturating_sub(half)..(i + half + 1).min(v.len())];
            s.iter().sum::<f32>() / s.len() as f32
        })
        .collect()
}

/// Renders the edits of a file: the file itself where nothing changed.
pub fn render(source: &Sample, analysis: &Analysis, edits: &[NoteEdit]) -> Sample {
    render_regions(source, analysis, edits, None, None, None).map(|(s, _)| s).unwrap_or_else(|| source.clone())
}

/// [`render`] starting from a previous render (`base`: its edits and its
/// samples), redoing only the regions whose edits changed. Returns the
/// render and the output spans (seconds) synthesized again; `None` when
/// cancelled. `progress` goes from 0 to 1000.
pub fn render_regions(
    source: &Sample,
    analysis: &Analysis,
    edits: &[NoteEdit],
    base: Option<(&[NoteEdit], &Sample)>,
    progress: Option<&AtomicU32>,
    cancel: Option<&AtomicBool>,
) -> Option<(Sample, Vec<(f64, f64)>)> {
    let sr = f64::from(source.sample_rate.max(1.0));
    let n = source.len();
    let right: &[f32] = if source.right.len() == n { &source.right } else { &source.left };
    let duration = n as f64 / sr;
    let map = TimeMap::new(edits, duration);
    let mut regions = spans(edits, &map, duration);
    let base = base.filter(|(_, s)| s.len() == n && s.right.len() == n);
    if let Some((old, _)) = base {
        let old_map = TimeMap::new(old, duration);
        let mut all = regions.clone();
        all.extend(spans(old, &old_map, duration));
        // Padded already: merging again only joins the overlapping ones.
        regions = merge(all.iter().map(|s| (s.0 + PAD, s.1 - PAD)).collect(), duration);
        regions.retain(|&r| touching(old, r) != touching(edits, r));
    }
    let (mut left_out, mut right_out) = match base {
        Some((_, s)) => (s.left.clone(), s.right.clone()),
        None => (source.left.clone(), right.to_vec()),
    };
    if !regions.is_empty() {
        let ctx = Ctx {
            src: [&source.left, right],
            mono: source.left.iter().zip(right).map(|(l, r)| 0.5 * (l + r)).collect(),
            sr,
            marks: Vec::new(),
            curves: curves(analysis, edits),
            map,
        };
        let ctx = Ctx { marks: marks(&ctx.mono, analysis), ..ctx };
        let total: f64 = regions.iter().map(|r| r.1 - r.0).sum::<f64>().max(1e-9);
        let mut done = 0.0;
        let mut buf = [Vec::new(), Vec::new()];
        for &(ra, rb) in &regions {
            if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                return None;
            }
            let (a, b) = ((ra * sr).floor() as i64, ((rb * sr).ceil() as i64).min(n as i64));
            let here = touching(edits, (ra, rb));
            let xf = (XFADE * sr) as i64;
            if here.iter().all(NoteEdit::is_identity) {
                // Back to the file.
                left_out[a as usize..b as usize].copy_from_slice(&source.left[a as usize..b as usize]);
                right_out[a as usize..b as usize].copy_from_slice(&right[a as usize..b as usize]);
            } else {
                synth(&ctx, a, b, &mut buf);
                let g = gains(&ctx, &here, a, b, &buf);
                for t in a..b {
                    let i = (t - a) as usize;
                    let edge = (t - a).min(b - 1 - t);
                    // At the file's own ends there is nothing to fade from.
                    let w = if (a == 0 && t - a < xf) || (b == n as i64 && b - 1 - t < xf) {
                        1.0
                    } else {
                        smoothstep(edge as f64 / xf as f64)
                    };
                    let k = t as usize;
                    left_out[k] = source.left[k] + (buf[0][i] * g[i] - source.left[k]) * w;
                    right_out[k] = right[k] + (buf[1][i] * g[i] - right[k]) * w;
                }
            }
            done += rb - ra;
            if let Some(p) = progress {
                p.store((1000.0 * done / total) as u32, Ordering::Relaxed);
            }
        }
    }
    let out = Sample { name: source.name.clone(), sample_rate: source.sample_rate, left: left_out, right: right_out, markers: source.markers.clone() };
    Some((out, regions))
}
