//! Pitch-synchronous overlap-add (TD-PSOLA), in real time.
//!
//! Analysis marks are placed one period apart on the voice, each aligned on
//! the previous one by cross-correlation (so the grains of a steady vowel are
//! coherent), the first one of a voiced run on the loudest point of its
//! period. A grain is two periods of input under a Hann window (its halves
//! as long as the distances to the neighbouring marks) centered on a mark.
//! Grains are laid down at the output pitch's period: a grain repeated or
//! skipped now and then keeps time. A grain is not resampled, so the spectral
//! envelope (the formants) stays where it was; reading it faster or slower
//! moves the formants on purpose.
//!
//! Where no grain is laid (unvoiced: s, t, breaths) the input passes as is,
//! with the latency: the grain windows themselves are the crossfades at the
//! edges of a voiced run. With no shift, grains rebuild the input exactly.

use super::frames::Frames;
use super::line::{Line, MONO};

#[derive(Clone, Copy, Debug, Default)]
struct Mark {
    pos: f64,
    period: f32,
    run: u32,
}

const MARKS: usize = 64;

#[derive(Clone, Copy, Debug, Default)]
struct Syn {
    active: bool,
    run: u32,
    first: bool,
    pick: u64,
    prev_s: f64,
    prev_r: f64,
}

pub struct Psola {
    sample_rate: f32,
    mask: usize,
    acc: [Vec<f32>; 2],
    cov: [Vec<f32>; 2],
    marks: [Mark; MARKS],
    count: u64,
    in_run: bool,
    run: u32,
    last: f64,
    last_period: f32,
    scan: f64,
    syn: [Syn; 2],
    latency: usize,
    p_max: f32,
    /// How far past the newest frame a lookup may go (low latency: any).
    ahead: f64,
}

impl Psola {
    pub fn new(sample_rate: f32, len: usize) -> Self {
        let n = len.next_power_of_two();
        Self {
            sample_rate,
            mask: n - 1,
            acc: [vec![0.0; n], vec![0.0; n]],
            cov: [vec![0.0; n], vec![0.0; n]],
            marks: [Mark::default(); MARKS],
            count: 0,
            in_run: false,
            run: 0,
            last: 0.0,
            last_period: 0.0,
            scan: 0.0,
            syn: [Syn::default(); 2],
            latency: 0,
            p_max: 1.0,
            ahead: 0.0,
        }
    }

    /// Latency `latency`, longest period `p_max` (samples), frame lookups up to `ahead` past the newest.
    pub fn configure(&mut self, latency: usize, p_max: f32, ahead: f64) {
        self.latency = latency;
        self.p_max = p_max;
        self.ahead = ahead;
        self.reset();
    }

    pub fn reset(&mut self) {
        self.acc.iter_mut().chain(self.cov.iter_mut()).for_each(|b| b.fill(0.0));
        self.count = 0;
        self.in_run = false;
        self.run = 0;
        self.scan = 0.0;
        self.syn = [Syn::default(); 2];
    }

    fn mark(&self, i: u64) -> Option<Mark> {
        (i < self.count && i + MARKS as u64 > self.count).then(|| self.marks[i as usize % MARKS])
    }

    fn push_mark(&mut self, pos: f64, period: f32, run: u32) {
        self.marks[self.count as usize % MARKS] = Mark { pos, period, run };
        self.count += 1;
    }

    /// Distance to the next mark of the run (the period when it is not known yet).
    fn fall(&self, i: u64) -> f64 {
        let Some(m) = self.mark(i) else { return 1.0 };
        match self.mark(i + 1) {
            Some(n) if n.run == m.run => n.pos - m.pos,
            _ => f64::from(m.period),
        }
    }

    /// Distance from the previous mark of the run.
    fn rise(&self, i: u64) -> f64 {
        let Some(m) = self.mark(i) else { return 1.0 };
        match i.checked_sub(1).and_then(|k| self.mark(k)) {
            Some(p) if p.run == m.run => m.pos - p.pos,
            _ => f64::from(m.period),
        }
    }

    fn period_of(&self, hz: f32) -> f32 {
        (self.sample_rate / hz.max(1.0)).clamp(8.0, self.p_max * 1.05)
    }

    /// Places the analysis marks the input allows so far.
    pub fn find_marks(&mut self, line: &Line, frames: &Frames, now: u64) {
        let newest = now as f64 - 1.0;
        let Some(latest) = frames.latest() else { return };
        let limit = (latest.time + self.ahead).min(newest);
        // Never look at what has already left.
        let gone = newest - self.latency as f64;
        for _ in 0..4 {
            if !self.in_run {
                if self.scan < gone {
                    self.scan = gone;
                }
                if self.scan > limit {
                    return;
                }
                let Some(f) = frames.at(self.scan) else { return };
                if !(f.voiced && f.hz > 0.0) {
                    self.scan += 16.0;
                    continue;
                }
                let p = self.period_of(f.hz);
                if self.scan + f64::from(p) + 2.0 > newest {
                    return;
                }
                // The loudest point of the first period.
                let start = self.scan.ceil() as i64;
                let mut best = (start, 0.0f32);
                for k in start..start + p.ceil() as i64 {
                    let v = line.get(MONO, k).abs();
                    if v > best.1 {
                        best = (k, v);
                    }
                }
                self.run = self.run.wrapping_add(1);
                self.in_run = true;
                self.last = best.0 as f64;
                self.last_period = p;
                self.push_mark(self.last, p, self.run);
            } else {
                let p = frames.at(self.last).filter(|f| f.hz > 0.0).map_or(self.last_period, |f| self.period_of(f.hz));
                let c = self.last + f64::from(p);
                let reach = (f64::from(p) * 0.25).ceil();
                if c + reach + 4.0 > newest || c > limit {
                    return;
                }
                if frames.at(c).is_none_or(|f| !f.voiced) {
                    self.in_run = false;
                    self.scan = c;
                    continue;
                }
                let delta = self.align(line, c, p, reach);
                let m = c + delta;
                self.last = m;
                self.last_period = p;
                self.push_mark(m, p, self.run);
            }
        }
    }

    /// Offset (within ±`reach`) that makes the period before `c` most like the
    /// period before the last mark.
    fn align(&self, line: &Line, c: f64, p: f32, reach: f64) -> f64 {
        let k_len = (p.round() as i64).clamp(8, 1_024);
        let base = c.round() as i64;
        let last = self.last.round() as i64;
        let reach = reach as i64;
        let score = |d: i64| -> f32 {
            let (mut xy, mut yy) = (0.0f32, 1e-9f32);
            for k in 1..=k_len {
                let y = line.get(MONO, base + d - k);
                xy += y * line.get(MONO, last - k);
                yy += y * y;
            }
            xy / yy.sqrt()
        };
        let step = ((p / 64.0).round() as i64).max(1);
        let mut best = (0i64, f32::MIN);
        let mut d = -reach;
        while d <= reach {
            let v = score(d);
            if v > best.1 {
                best = (d, v);
            }
            d += step;
        }
        if step > 1 {
            let centre = best.0;
            for d in (centre - step + 1)..(centre + step) {
                if d != centre && d.abs() <= reach {
                    let v = score(d);
                    if v > best.1 {
                        best = (d, v);
                    }
                }
            }
        }
        let (a, b, cc) = (score(best.0 - 1), best.1, score(best.0 + 1));
        let den = a - 2.0 * b + cc;
        let frac = if den < 0.0 { (0.5 * (a - cc) / den).clamp(-0.5, 0.5) } else { 0.0 };
        base as f64 + best.0 as f64 + f64::from(frac) - c
    }

    /// Lays the grains of channel `ch` due now. `spread`: ratio applied on top
    /// of the correction (stereo width); `formant`: factor on the formants
    /// (1: preserved), or `None` to let them follow the pitch.
    pub fn synth(&mut self, ch: usize, line: &Line, frames: &Frames, now: u64, spread: f64, formant: Option<f64>) {
        let newest = now as f64 - 1.0;
        let lat = self.latency as f64;
        let out = newest - lat;
        for _ in 0..6 {
            let mut syn = self.syn[ch];
            if !syn.active {
                let oldest = self.count.saturating_sub(MARKS as u64 - 1);
                let Some(i) = (oldest..self.count).find(|&i| self.mark(i).is_some_and(|m| m.run.wrapping_sub(syn.run) as i32 > 0)) else { return };
                let m = self.marks[i as usize % MARKS];
                syn = Syn { active: true, run: m.run, first: true, pick: i, prev_s: m.pos, prev_r: 1.0 };
                self.syn[ch] = syn;
            }
            let s = if syn.first { self.marks[syn.pick as usize % MARKS].pos } else { syn.prev_s + self.fall(syn.pick) / syn.prev_r };
            if newest < s + lat - f64::from(self.p_max) {
                return;
            }
            let pick = if syn.first {
                Some(syn.pick)
            } else {
                let j = syn.pick;
                let run_open = self.in_run && self.run == syn.run;
                let next = self.mark(j + 1).filter(|m| m.run == syn.run);
                if next.is_none() && !run_open {
                    self.syn[ch].active = false;
                    continue;
                }
                let ok = |i: u64| {
                    self.mark(i).filter(|m| m.run == syn.run).filter(|m| m.pos + self.fall(i) + 3.0 <= newest).map(|m| (i, m.pos))
                };
                let p = f64::from(self.marks[j as usize % MARKS].period);
                [ok(j), ok(j + 1), ok(j + 2)]
                    .into_iter()
                    .flatten()
                    .min_by(|a, b| {
                        let cost = |(i, pos): (u64, f64)| (pos - s).abs() - if i == j + 1 { 0.1 * p } else { 0.0 };
                        cost(*a).total_cmp(&cost(*b))
                    })
                    .map(|(i, _)| i)
            };
            let Some(i) = pick else {
                self.syn[ch].active = false;
                return;
            };
            let a = self.marks[i as usize % MARKS].pos;
            let r = (2f64.powf(f64::from(frames.shift_at(a)) / 12.0) * spread).clamp(0.25, 4.0);
            let phi = formant.map_or(r, |f| f).clamp(0.25, 4.0);
            let gain = (phi / r).sqrt() as f32;
            self.grain(ch, line, a, self.rise(i), self.fall(i), s, phi, gain, syn.first, syn.prev_s, out);
            self.syn[ch] = Syn { active: true, run: syn.run, first: false, pick: i, prev_s: s, prev_r: r };
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn grain(&mut self, ch: usize, line: &Line, a: f64, rise: f64, fall: f64, s: f64, phi: f64, gain: f32, first: bool, prev_s: f64, out: f64) {
        let (r_out, f_out) = (rise / phi, fall / phi);
        let start = (s - r_out).ceil().max(out);
        let end = (s + f_out).floor();
        let m = self.mask;
        let mut t = start;
        while t <= end {
            let d = t - s;
            let w = if d < 0.0 { 0.5 + 0.5 * (std::f64::consts::PI * d / r_out).cos() } else { 0.5 + 0.5 * (std::f64::consts::PI * d / f_out).cos() };
            let x = line.interp(ch, a + d * phi);
            let idx = t as i64 as usize & m;
            self.acc[ch][idx] += w as f32 * x * gain;
            let cv = if d < 0.0 && !first { 1.0 } else { w as f32 };
            if self.cov[ch][idx] < cv {
                self.cov[ch][idx] = cv;
            }
            t += 1.0;
        }
        // Between the previous grain and this one, the grains cover everything.
        if !first {
            let mut t = prev_s.ceil().max(out);
            while t < start {
                self.cov[ch][t as i64 as usize & m] = 1.0;
                t += 1.0;
            }
        }
    }

    /// Output sample of channel `ch` leaving now.
    #[inline]
    pub fn output(&mut self, ch: usize, line: &Line, now: u64) -> f32 {
        let o = now as i64 - 1 - self.latency as i64;
        if o < 0 {
            return 0.0;
        }
        let idx = o as usize & self.mask;
        let v = self.acc[ch][idx] + (1.0 - self.cov[ch][idx].min(1.0)) * line.get(ch, o);
        self.acc[ch][idx] = 0.0;
        self.cov[ch][idx] = 0.0;
        v
    }
}
