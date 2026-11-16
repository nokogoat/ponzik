//! Tempo map and time signature map: changes of tempo (instant or linear
//! ramps) and of meter along the song timeline.
//!
//! Conversions between ticks and seconds are exact: inside a ramp the tempo
//! moves linearly with the ticks, so the time it takes is a logarithm and its
//! inverse an exponential, both in closed form. Nothing here allocates once the
//! map is built: the engine queries it from the audio thread.

use crate::sequence::PPQ;

/// Slowest and fastest tempos the engine plays.
pub const MIN_BPM: f64 = 20.0;
pub const MAX_BPM: f64 = 999.0;

/// A tempo change at `tick`. With `ramp`, the tempo glides linearly (in ticks)
/// from the previous point's tempo to this one instead of jumping here.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TempoPoint {
    pub tick: u32,
    pub bpm: f32,
    pub ramp: bool,
}

/// A time signature starting at `tick`: `num` beats of a `den`th note per bar.
/// Bars restart at every signature change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimeSig {
    pub tick: u32,
    pub num: u8,
    pub den: u8,
}

impl TimeSig {
    pub const FOUR_FOUR: TimeSig = TimeSig { tick: 0, num: 4, den: 4 };

    /// Ticks of one beat (a `den`th note).
    pub fn beat_len(self) -> u32 {
        PPQ * 4 / self.den.max(1) as u32
    }

    /// Ticks of one full bar.
    pub fn bar_len(self) -> u32 {
        self.beat_len() * self.num.max(1) as u32
    }

    /// Same meter (position ignored).
    pub fn same_meter(self, other: TimeSig) -> bool {
        (self.num, self.den) == (other.num, other.den)
    }

    /// Valid denominators: powers of two from 1 to 32.
    pub fn valid_den(den: u8) -> bool {
        matches!(den, 1 | 2 | 4 | 8 | 16 | 32)
    }
}

/// Position in bars and beats (all 0-based).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BarBeat {
    pub bar: u32,
    pub beat: u32,
    /// Ticks since the start of the beat.
    pub tick_in_beat: f64,
    pub sig: TimeSig,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TempoMap {
    /// Sorted by tick, the first one at tick 0 (never a ramp).
    tempos: Vec<TempoPoint>,
    /// Seconds elapsed at each tempo point.
    secs: Vec<f64>,
    /// Sorted by tick, the first one at tick 0.
    sigs: Vec<TimeSig>,
    /// Index of the first bar of each signature.
    bars: Vec<u32>,
}

impl Default for TempoMap {
    fn default() -> Self {
        Self::constant(120.0)
    }
}

/// Borrowed view of a map: the whole map, or only its first tempo and
/// signature (patterns and session clips loop outside the song timeline).
#[derive(Clone, Copy, Debug)]
pub struct TempoView<'a> {
    tempos: &'a [TempoPoint],
    secs: &'a [f64],
    sigs: &'a [TimeSig],
    bars: &'a [u32],
}

fn clamp_bpm(bpm: f32) -> f64 {
    let bpm = bpm as f64;
    if bpm.is_finite() { bpm.clamp(MIN_BPM, MAX_BPM) } else { 120.0 }
}

/// Ticks per second at `bpm`.
fn rate(bpm: f64) -> f64 {
    bpm * PPQ as f64 / 60.0
}

/// Below this slope (BPM per tick) a ramp is treated as constant.
const FLAT: f64 = 1e-12;

impl TempoMap {
    /// One tempo, 4/4.
    pub fn constant(bpm: f32) -> Self {
        Self::new(vec![TempoPoint { tick: 0, bpm, ramp: false }], Vec::new())
    }

    /// Builds a map from changes in any order. Points on the same tick: the
    /// last one wins. Without a point at tick 0 the first tempo (120 BPM if
    /// none) and 4/4 start there.
    pub fn new(mut tempos: Vec<TempoPoint>, mut sigs: Vec<TimeSig>) -> Self {
        tempos.retain(|p| p.bpm.is_finite());
        tempos.sort_by_key(|p| p.tick);
        tempos.dedup_by(|later, earlier| {
            if later.tick == earlier.tick {
                *earlier = *later;
                true
            } else {
                false
            }
        });
        for p in &mut tempos {
            p.bpm = clamp_bpm(p.bpm) as f32;
        }
        match tempos.first() {
            Some(p) if p.tick == 0 => {}
            Some(p) => {
                let bpm = p.bpm;
                tempos.insert(0, TempoPoint { tick: 0, bpm, ramp: false });
            }
            None => tempos.push(TempoPoint { tick: 0, bpm: 120.0, ramp: false }),
        }
        tempos[0].ramp = false;

        sigs.retain(|s| s.num >= 1 && TimeSig::valid_den(s.den));
        sigs.sort_by_key(|s| s.tick);
        sigs.dedup_by(|later, earlier| {
            if later.tick == earlier.tick {
                *earlier = *later;
                true
            } else {
                false
            }
        });
        if sigs.first().is_none_or(|s| s.tick != 0) {
            sigs.insert(0, TimeSig::FOUR_FOUR);
        }
        let mut map = Self { secs: Vec::with_capacity(tempos.len()), tempos, bars: Vec::with_capacity(sigs.len()), sigs };
        map.update_secs();
        let mut bar = 0;
        for (i, s) in map.sigs.iter().enumerate() {
            if i > 0 {
                let prev = map.sigs[i - 1];
                bar += (s.tick - prev.tick).div_ceil(prev.bar_len());
            }
            map.bars.push(bar);
        }
        map
    }

    fn update_secs(&mut self) {
        self.secs.clear();
        let mut acc = 0.0;
        for i in 0..self.tempos.len() {
            if i > 0 {
                acc += segment_secs(&self.tempos, i - 1, self.tempos[i].tick as f64);
            }
            self.secs.push(acc);
        }
    }

    /// Changes the first tempo in place (no allocation).
    pub fn set_first_bpm(&mut self, bpm: f32) {
        self.tempos[0].bpm = clamp_bpm(bpm) as f32;
        self.update_secs();
    }

    pub fn tempos(&self) -> &[TempoPoint] {
        &self.tempos
    }

    pub fn sigs(&self) -> &[TimeSig] {
        &self.sigs
    }

    /// The whole map.
    pub fn view(&self) -> TempoView<'_> {
        TempoView { tempos: &self.tempos, secs: &self.secs, sigs: &self.sigs, bars: &self.bars }
    }

    /// Only the first tempo and signature, held forever.
    pub fn head(&self) -> TempoView<'_> {
        TempoView { tempos: &self.tempos[..1], secs: &self.secs[..1], sigs: &self.sigs[..1], bars: &self.bars[..1] }
    }

    pub fn first_bpm(&self) -> f64 {
        self.tempos[0].bpm as f64
    }

    pub fn bpm_at(&self, tick: f64) -> f64 {
        self.view().bpm_at(tick)
    }
    pub fn secs_at(&self, tick: f64) -> f64 {
        self.view().secs_at(tick)
    }
    pub fn tick_at_secs(&self, secs: f64) -> f64 {
        self.view().tick_at_secs(secs)
    }
    pub fn sig_at(&self, tick: f64) -> TimeSig {
        self.view().sig_at(tick)
    }
    pub fn bar_beat(&self, tick: f64) -> BarBeat {
        self.view().bar_beat(tick)
    }
    pub fn bar_start(&self, bar: u32) -> u32 {
        self.view().bar_start(bar)
    }
    pub fn bar_at(&self, tick: f64) -> u32 {
        self.view().bar_beat(tick).bar
    }
    /// Nearest bar start.
    pub fn snap_bar(&self, tick: f64) -> u32 {
        self.view().snap_bar(tick)
    }
    /// Bar start at or after `tick`.
    pub fn ceil_bar(&self, tick: f64) -> u32 {
        self.view().ceil_bar(tick)
    }
    pub fn snap_beat(&self, tick: f64) -> u32 {
        self.view().snap_beat(tick)
    }
    /// Frames from `a` to `b` (ticks) at `sample_rate`, rounded the way the
    /// engine cuts its blocks at the end of a loop.
    pub fn frames_between(&self, a: f64, b: f64, sample_rate: f64) -> usize {
        (((self.secs_at(b) - self.secs_at(a)) * sample_rate) - 1e-6).ceil().max(0.0) as usize
    }
    pub fn for_each_beat(&self, from: f64, to: f64, f: impl FnMut(f64, bool)) {
        self.view().for_each_beat(from, to, f)
    }
    pub fn for_each_bar(&self, from: f64, to: f64, f: impl FnMut(u32, u32)) {
        self.view().for_each_bar(from, to, f)
    }
}

/// Seconds from the start of tempo segment `i` to `tick` (inside it, or past
/// its end when it is the last one).
fn segment_secs(tempos: &[TempoPoint], i: usize, tick: f64) -> f64 {
    let a = tempos[i];
    let t0 = a.tick as f64;
    let b0 = a.bpm as f64;
    let dt = tick - t0;
    match tempos.get(i + 1) {
        Some(b) if b.ramp && b.tick > a.tick => {
            let k = (b.bpm as f64 - b0) / (b.tick as f64 - t0);
            if k.abs() < FLAT {
                dt / rate(b0)
            } else {
                // dτ/dt = (b0 + kτ)·PPQ/60  ⇒  t = 60/(k·PPQ) · ln((b0 + kτ)/b0)
                60.0 / (k * PPQ as f64) * (k * dt / b0).ln_1p()
            }
        }
        _ => dt / rate(b0),
    }
}

impl TempoView<'_> {
    /// Tempo segment containing `tick` (the first one before 0).
    fn segment(&self, tick: f64) -> usize {
        self.tempos.partition_point(|p| p.tick as f64 <= tick).saturating_sub(1)
    }

    fn ramp_slope(&self, i: usize) -> Option<f64> {
        let a = self.tempos[i];
        let b = self.tempos.get(i + 1)?;
        (b.ramp && b.tick > a.tick).then(|| (b.bpm as f64 - a.bpm as f64) / (b.tick as f64 - a.tick as f64))
    }

    /// Tempo at `tick`.
    pub fn bpm_at(&self, tick: f64) -> f64 {
        let i = self.segment(tick);
        let a = self.tempos[i];
        match self.ramp_slope(i) {
            Some(k) => a.bpm as f64 + k * (tick - a.tick as f64).max(0.0),
            None => a.bpm as f64,
        }
    }

    /// Seconds from tick 0 to `tick` (negative before 0, at the first tempo).
    pub fn secs_at(&self, tick: f64) -> f64 {
        if tick <= 0.0 {
            return tick / rate(self.tempos[0].bpm as f64);
        }
        let i = self.segment(tick);
        self.secs[i] + segment_secs(self.tempos, i, tick)
    }

    /// Inverse of [`TempoView::secs_at`].
    pub fn tick_at_secs(&self, secs: f64) -> f64 {
        if secs <= 0.0 {
            return secs * rate(self.tempos[0].bpm as f64);
        }
        let i = self.secs.partition_point(|&s| s <= secs).saturating_sub(1);
        let a = self.tempos[i];
        let (t0, b0) = (a.tick as f64, a.bpm as f64);
        let ds = secs - self.secs[i];
        let tick = match self.ramp_slope(i) {
            Some(k) if k.abs() >= FLAT => t0 + b0 * (k * PPQ as f64 * ds / 60.0).exp_m1() / k,
            _ => t0 + ds * rate(b0),
        };
        // Rounding can't push a tick past the next point.
        match self.tempos.get(i + 1) {
            Some(b) => tick.min(b.tick as f64),
            None => tick,
        }
    }

    fn sig_index(&self, tick: f64) -> usize {
        self.sigs.partition_point(|s| s.tick as f64 <= tick).saturating_sub(1)
    }

    pub fn sig_at(&self, tick: f64) -> TimeSig {
        self.sigs[self.sig_index(tick)]
    }

    pub fn bar_beat(&self, tick: f64) -> BarBeat {
        let tick = tick.max(0.0);
        let i = self.sig_index(tick);
        let sig = self.sigs[i];
        let rel = tick - sig.tick as f64;
        let bar_len = sig.bar_len() as f64;
        let in_bar = (rel / bar_len).floor();
        let within = rel - in_bar * bar_len;
        let beat_len = sig.beat_len() as f64;
        let beat = (within / beat_len).floor().min(sig.num as f64 - 1.0);
        BarBeat { bar: self.bars[i] + in_bar as u32, beat: beat as u32, tick_in_beat: within - beat * beat_len, sig }
    }

    /// First tick of bar `bar` (0-based).
    pub fn bar_start(&self, bar: u32) -> u32 {
        let i = self.bars.partition_point(|&b| b <= bar).saturating_sub(1);
        let sig = self.sigs[i];
        sig.tick.saturating_add((bar - self.bars[i]).saturating_mul(sig.bar_len()))
    }

    /// Nearest bar start.
    pub fn snap_bar(&self, tick: f64) -> u32 {
        let bar = self.bar_beat(tick).bar;
        let (a, b) = (self.bar_start(bar), self.bar_start(bar + 1));
        if tick - a as f64 <= b as f64 - tick { a } else { b }
    }

    /// Bar start at or after `tick`.
    pub fn ceil_bar(&self, tick: f64) -> u32 {
        let bar = self.bar_beat(tick).bar;
        let a = self.bar_start(bar);
        if a as f64 >= tick { a } else { self.bar_start(bar + 1) }
    }

    /// Nearest beat (beats restart at every bar).
    pub fn snap_beat(&self, tick: f64) -> u32 {
        let tick = tick.max(0.0);
        let sig = self.sig_at(tick);
        let beat = sig.beat_len() as f64;
        let rel = ((tick - sig.tick as f64) / beat).round() * beat;
        let snapped = (sig.tick as f64 + rel) as u32;
        // A rounded beat past a signature change lands on the change.
        let next = self.sigs.get(self.sig_index(tick) + 1).map_or(u32::MAX, |s| s.tick);
        snapped.min(next)
    }

    /// Calls `f(tick, downbeat)` for every beat in [`from`, `to`).
    pub fn for_each_beat(&self, from: f64, to: f64, mut f: impl FnMut(f64, bool)) {
        let mut i = self.sig_index(from.max(0.0));
        while i < self.sigs.len() {
            let sig = self.sigs[i];
            let start = sig.tick as f64;
            if start >= to {
                break;
            }
            let end = self.sigs.get(i + 1).map_or(f64::INFINITY, |s| s.tick as f64).min(to);
            let beat = sig.beat_len() as f64;
            let mut k = ((from.max(start) - start) / beat - 1e-9).ceil().max(0.0) as u64;
            loop {
                let tick = start + k as f64 * beat;
                if tick >= end {
                    break;
                }
                f(tick, k % sig.num.max(1) as u64 == 0);
                k += 1;
            }
            i += 1;
        }
    }

    /// Calls `f(bar, tick)` for every bar start in [`from`, `to`].
    pub fn for_each_bar(&self, from: f64, to: f64, mut f: impl FnMut(u32, u32)) {
        let mut bar = self.bar_beat(from).bar;
        loop {
            let tick = self.bar_start(bar);
            if tick as f64 > to {
                break;
            }
            if tick as f64 >= from {
                f(bar, tick);
            }
            bar += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tp(tick: u32, bpm: f32, ramp: bool) -> TempoPoint {
        TempoPoint { tick, bpm, ramp }
    }

    fn sig(tick: u32, num: u8, den: u8) -> TimeSig {
        TimeSig { tick, num, den }
    }

    #[test]
    fn constant_tempo_is_linear() {
        let m = TempoMap::constant(120.0);
        // 120 BPM: 192 ticks per second.
        assert!((m.secs_at(192.0) - 1.0).abs() < 1e-12);
        assert!((m.tick_at_secs(2.5) - 480.0).abs() < 1e-9);
        assert!((m.secs_at(-96.0) + 0.5).abs() < 1e-12);
        assert_eq!(m.frames_between(0.0, 384.0, 48_000.0), 96_000);
    }

    #[test]
    fn ramps_round_trip_and_stay_monotonic() {
        let m = TempoMap::new(
            vec![tp(0, 100.0, false), tp(384, 100.0, false), tp(768, 200.0, true), tp(1152, 60.0, true), tp(1536, 140.0, false)],
            Vec::new(),
        );
        let mut last_s = f64::NEG_INFINITY;
        let mut tick = -50.0;
        while tick < 2_500.0 {
            let s = m.secs_at(tick);
            assert!(s > last_s, "secs not increasing at {tick}");
            last_s = s;
            let back = m.tick_at_secs(s);
            assert!((back - tick).abs() < 1e-6, "{tick} → {s} → {back}");
            tick += 0.73;
        }
        // Constant first segment: 384 ticks at 100 BPM = 2.4 s.
        assert!((m.secs_at(384.0) - 2.4).abs() < 1e-12);
        // Ramp 100 → 200 BPM over 384 ticks: 60/(k·PPQ)·ln 2 with k = 100/384.
        let k = 100.0 / 384.0;
        let expected = 2.4 + 60.0 / (k * 96.0) * 2f64.ln();
        assert!((m.secs_at(768.0) - expected).abs() < 1e-9);
        // The ramp sits between the two constant tempos it joins.
        let ramp = m.secs_at(768.0) - m.secs_at(384.0);
        assert!(ramp < 384.0 / rate(100.0) && ramp > 384.0 / rate(200.0));
        assert!((m.bpm_at(576.0) - 150.0).abs() < 1e-9);
        assert!((m.bpm_at(960.0) - 130.0).abs() < 1e-9);
        // An instant change: the tempo jumps at the point.
        assert_eq!(m.bpm_at(1535.9), 60.0);
        assert_eq!(m.bpm_at(1536.0), 140.0);
        // Frames are additive (to the sample) across segments.
        let sr = 48_000.0;
        let whole = (m.secs_at(1536.0) * sr).ceil();
        assert_eq!(m.frames_between(0.0, 1536.0, sr) as f64, whole);
    }

    #[test]
    fn first_point_is_forced_to_zero_and_never_ramps() {
        let m = TempoMap::new(vec![tp(96, 90.0, true)], Vec::new());
        assert_eq!(m.tempos()[0], tp(0, 90.0, false));
        assert_eq!(m.tempos().len(), 2);
        assert_eq!(m.bpm_at(48.0), 90.0);
        let mut m = TempoMap::new(vec![tp(0, 5.0, false)], Vec::new());
        assert_eq!(m.first_bpm(), MIN_BPM);
        m.set_first_bpm(150.0);
        assert!((m.secs_at(240.0) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn bars_and_beats_follow_mixed_signatures() {
        // 2 bars of 4/4, 3 bars of 3/4, 2 bars of 6/8, then 7/8.
        let m = TempoMap::new(Vec::new(), vec![sig(0, 4, 4), sig(768, 3, 4), sig(1632, 6, 8), sig(2208, 7, 8)]);
        let at = |t: f64| {
            let b = m.bar_beat(t);
            (b.bar, b.beat)
        };
        assert_eq!(at(0.0), (0, 0));
        assert_eq!(at(767.0), (1, 3));
        assert_eq!(at(768.0), (2, 0));
        assert_eq!(at(768.0 + 288.0 + 96.0), (3, 1));
        assert_eq!(at(1632.0), (5, 0));
        assert_eq!(at(1632.0 + 48.0 * 5.0), (5, 5));
        assert_eq!(at(1632.0 + 288.0), (6, 0));
        assert_eq!(at(2208.0), (7, 0));
        assert_eq!(at(2208.0 + 336.0 + 48.0), (8, 1));
        let starts: Vec<u32> = (0..10).map(|b| m.bar_start(b)).collect();
        assert_eq!(starts, [0, 384, 768, 1056, 1344, 1632, 1920, 2208, 2544, 2880]);
        assert_eq!(m.snap_bar(1000.0), 1056);
        assert_eq!(m.snap_bar(800.0), 768);
        assert_eq!(m.ceil_bar(769.0), 1056);
        assert_eq!(m.ceil_bar(768.0), 768);
        let mut bars = Vec::new();
        m.for_each_bar(700.0, 1700.0, |b, t| bars.push((b, t)));
        assert_eq!(bars, [(2, 768), (3, 1056), (4, 1344), (5, 1632)]);
        // A change off the bar grid starts a new bar; the cut bar still counts.
        let m = TempoMap::new(Vec::new(), vec![sig(0, 4, 4), sig(480, 5, 4)]);
        assert_eq!(m.bar_beat(479.0).bar, 1);
        assert_eq!(m.bar_beat(480.0).bar, 2);
        assert_eq!(m.bar_start(3), 960);
    }

    #[test]
    fn beats_and_downbeats_follow_the_signature() {
        let m = TempoMap::new(Vec::new(), vec![sig(0, 3, 4), sig(576, 6, 8)]);
        let mut beats = Vec::new();
        m.for_each_beat(0.0, 576.0 + 288.0 + 1.0, |t, down| beats.push((t as u32, down)));
        let expected: Vec<(u32, bool)> = vec![
            (0, true),
            (96, false),
            (192, false),
            (288, true),
            (384, false),
            (480, false),
            (576, true),
            (624, false),
            (672, false),
            (720, false),
            (768, false),
            (816, false),
            (864, true),
        ];
        assert_eq!(beats, expected);
        // A window starting between beats skips the one before it.
        let mut later = Vec::new();
        m.for_each_beat(100.0, 300.0, |t, d| later.push((t as u32, d)));
        assert_eq!(later, [(192, false), (288, true)]);
        assert_eq!(m.snap_beat(640.0), 624);
    }

    #[test]
    fn head_view_ignores_later_changes() {
        let m = TempoMap::new(vec![tp(0, 120.0, false), tp(384, 60.0, false)], vec![sig(0, 4, 4), sig(384, 3, 4)]);
        let h = m.head();
        assert_eq!(h.bpm_at(1000.0), 120.0);
        assert_eq!(h.sig_at(1000.0), sig(0, 4, 4));
        assert!((h.secs_at(768.0) - 4.0).abs() < 1e-12);
        assert!((m.secs_at(768.0) - 6.0).abs() < 1e-12);
    }

    #[test]
    fn invalid_points_are_dropped() {
        let m = TempoMap::new(vec![tp(0, f32::NAN, false), tp(10, 130.0, false), tp(10, 140.0, false)], vec![sig(0, 0, 4), sig(0, 3, 3)]);
        assert_eq!(m.tempos(), [tp(0, 140.0, false), tp(10, 140.0, false)]);
        assert_eq!(m.sigs(), [TimeSig::FOUR_FOUR]);
    }
}
