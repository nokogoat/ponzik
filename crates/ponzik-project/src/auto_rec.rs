//! Live automation recording: parameter moves made while the song plays become
//! points of its automation lanes, in Touch, Latch or Write mode.
//!
//! The interface reports moves ([`Recorder::touch`]) and calls
//! [`Recorder::tick`] every frame; the recorded passes stay here until
//! [`Recorder::finish`] writes them, thinned, into the project lanes at once.

use ponzik_engine::sequence::automation_value;
use ponzik_engine::{AutoPoint, AutoScope, AutoTarget};

use crate::{AutomationDef, Project};

/// What moving a parameter does while the song plays.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AutoMode {
    /// Automation plays back, moves are not recorded.
    #[default]
    Read,
    /// Records while the control is held; on release, the lane returns to
    /// what it was.
    Touch,
    /// Records from the first move until the transport stops.
    Latch,
    /// Armed lanes are overwritten from the start of playback, moved or not.
    Write,
}

impl AutoMode {
    pub const ALL: [AutoMode; 4] = [Self::Read, Self::Touch, Self::Latch, Self::Write];

    pub fn id(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Touch => "touch",
            Self::Latch => "latch",
            Self::Write => "write",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.id() == id)
    }
}

/// After a Touch release, ticks to glide back to the lane's old curve.
pub const RETURN_TICKS: u32 = 48;

/// One continuous stretch of recorded points.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Segment {
    pub points: Vec<AutoPoint>,
    /// Ended by a Touch release: the old curve resumes right after.
    pub returns: bool,
}

/// What a pass records for one parameter.
#[derive(Clone, Debug, PartialEq)]
pub struct LaneWrite {
    pub target: AutoTarget,
    /// Thinning tolerance, in the parameter's units.
    pub tolerance: f32,
    pub segments: Vec<Segment>,
    /// A segment is being written.
    pub open: bool,
    /// Keeps writing without being touched (Latch, Write).
    latched: bool,
}

impl LaneWrite {
    fn append(&mut self, tick: u32, value: f32) {
        if !self.open {
            self.segments.push(Segment::default());
            self.open = true;
        }
        let seg = self.segments.last_mut().expect("open segment");
        match seg.points.last_mut() {
            Some(last) if last.tick == tick => last.value = value,
            // The transport looped back: a new stretch from here.
            Some(last) if tick < last.tick => {
                self.segments.push(Segment { points: vec![AutoPoint { tick, value, curve: 0.0 }], returns: false });
            }
            _ => seg.points.push(AutoPoint { tick, value, curve: 0.0 }),
        }
    }
}

/// A recording pass: from play to stop.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Recorder {
    pub mode: AutoMode,
    pub writes: Vec<LaneWrite>,
}

impl Recorder {
    pub fn new(mode: AutoMode) -> Self {
        Self { mode, writes: Vec::new() }
    }

    pub fn is_empty(&self) -> bool {
        self.writes.iter().all(|w| w.segments.iter().all(|s| s.points.is_empty()))
    }

    pub fn write(&self, target: AutoTarget) -> Option<&LaneWrite> {
        self.writes.iter().find(|w| w.target == target)
    }

    /// Targets being written right now (the engine must not play them back).
    pub fn open_targets(&self) -> impl Iterator<Item = AutoTarget> + '_ {
        self.writes.iter().filter(|w| w.open).map(|w| w.target)
    }

    fn lane(&mut self, target: AutoTarget, tolerance: f32) -> &mut LaneWrite {
        let i = match self.writes.iter().position(|w| w.target == target) {
            Some(i) => i,
            None => {
                self.writes.push(LaneWrite { target, tolerance, segments: Vec::new(), open: false, latched: false });
                self.writes.len() - 1
            }
        };
        &mut self.writes[i]
    }

    /// The parameter was moved to `value` at `tick`. Returns true if it starts
    /// a new stretch (the engine should stop playing the lane back).
    pub fn touch(&mut self, target: AutoTarget, tick: u32, value: f32, tolerance: f32) -> bool {
        if self.mode == AutoMode::Read {
            return false;
        }
        let latch = self.mode != AutoMode::Touch;
        let w = self.lane(target, tolerance);
        let starts = !w.open;
        w.latched |= latch;
        w.append(tick, value);
        starts
    }

    /// Write mode: armed lanes are written from the start of playback.
    pub fn start_write(&mut self, targets: &[(AutoTarget, f32, f32)], tick: u32) {
        if self.mode != AutoMode::Write {
            return;
        }
        for &(target, value, tolerance) in targets {
            self.touch(target, tick, value, tolerance);
        }
    }

    /// One frame of playback: open lanes keep the current value, Touch lanes
    /// no longer held are released. Returns the released targets.
    pub fn tick(&mut self, tick: u32, value_of: impl Fn(AutoTarget) -> Option<f32>, held: impl Fn(AutoTarget) -> bool) -> Vec<AutoTarget> {
        let mut released = Vec::new();
        for w in self.writes.iter_mut().filter(|w| w.open) {
            if !w.latched && !held(w.target) {
                w.open = false;
                if let Some(seg) = w.segments.last_mut() {
                    seg.returns = true;
                }
                released.push(w.target);
                continue;
            }
            if let Some(v) = value_of(w.target) {
                w.append(tick, v);
            }
        }
        released
    }

    /// Ends the pass: every stretch, thinned, replaces what the song lanes
    /// had over its span (lanes created as needed). Returns the written targets.
    pub fn finish(&mut self, project: &mut Project) -> Vec<AutoTarget> {
        let mut done = Vec::new();
        for w in std::mem::take(&mut self.writes) {
            let segments: Vec<&Segment> = w.segments.iter().filter(|s| !s.points.is_empty()).collect();
            if segments.is_empty() {
                continue;
            }
            if project.automation_lane(w.target, AutoScope::Song).is_none() {
                project.automation.push(AutomationDef { target: w.target, scope: AutoScope::Song, points: Vec::new() });
            }
            let lane = project.automation_lane_mut(w.target, AutoScope::Song).expect("lane created");
            for seg in segments {
                let thinned = thin(&seg.points, w.tolerance);
                punch(&mut lane.points, &thinned, seg.returns.then_some(RETURN_TICKS));
            }
            done.push(w.target);
        }
        done
    }
}

/// Points of `seg` replace those of `points` over its span. A guard point
/// keeps the old curve up to the start; with `ret`, the old curve comes back
/// `ret` ticks after the end.
pub fn punch(points: &mut Vec<AutoPoint>, seg: &[AutoPoint], ret: Option<u32>) {
    let (Some(first), Some(last)) = (seg.first(), seg.last()) else { return };
    let (a, b) = (first.tick, last.tick);
    let had = !points.is_empty();
    let before = automation_value(points, a as f64);
    let end = b.saturating_add(ret.unwrap_or(0));
    let after = ret.and_then(|_| automation_value(points, end as f64));
    points.retain(|p| p.tick < a || p.tick > end);
    if had
        && a > 0
        && let Some(v) = before
        && (v - first.value).abs() > f32::EPSILON
    {
        points.push(AutoPoint { tick: a - 1, value: v, curve: 0.0 });
    }
    points.extend_from_slice(seg);
    if let Some(v) = after
        && ret.is_some_and(|r| r > 0)
    {
        points.push(AutoPoint { tick: end, value: v, curve: 0.0 });
    }
    points.sort_by_key(|p| p.tick);
    points.dedup_by(|later, earlier| {
        // Same tick: the later point (the recorded one) wins.
        if later.tick == earlier.tick {
            *earlier = *later;
            true
        } else {
            false
        }
    });
}

/// Fewer points, same shape: Ramer–Douglas–Peucker with a value tolerance
/// (the distance is measured vertically, in the parameter's units).
pub fn thin(points: &[AutoPoint], tolerance: f32) -> Vec<AutoPoint> {
    if points.len() <= 2 {
        return points.to_vec();
    }
    let mut keep = vec![false; points.len()];
    keep[0] = true;
    keep[points.len() - 1] = true;
    let mut stack = vec![(0, points.len() - 1)];
    while let Some((i, j)) = stack.pop() {
        if j <= i + 1 {
            continue;
        }
        let (a, b) = (points[i], points[j]);
        let span = (b.tick - a.tick) as f32;
        let mut worst = (0.0f32, i);
        for (k, p) in points.iter().enumerate().take(j).skip(i + 1) {
            let t = if span > 0.0 { (p.tick - a.tick) as f32 / span } else { 0.0 };
            let d = (a.value + (b.value - a.value) * t - p.value).abs();
            if d > worst.0 {
                worst = (d, k);
            }
        }
        if worst.0 > tolerance {
            keep[worst.1] = true;
            stack.push((i, worst.1));
            stack.push((worst.1, j));
        }
    }
    points.iter().zip(keep).filter(|(_, k)| *k).map(|(p, _)| *p).collect()
}

/// Parameters whose value differs between two states of the same project
/// (devices, volumes, pans), with their new values. Empty when the structure
/// changed (channels added, devices replaced).
pub fn changed_targets(old: &Project, new: &Project) -> Vec<(AutoTarget, f32)> {
    let mut out = Vec::new();
    if old.channels.len() != new.channels.len() || old.tracks.len() != new.tracks.len() {
        return out;
    }
    fn params(a: &[(u32, f32)], b: &[(u32, f32)], mut push: impl FnMut(u32, f32)) {
        for &(id, v) in b {
            if a.iter().find(|(i, _)| *i == id).is_none_or(|(_, o)| *o != v) {
                push(id, v);
            }
        }
    }
    for (channel, (a, b)) in old.channels.iter().zip(&new.channels).enumerate() {
        if a.instrument.kind == b.instrument.kind {
            params(&a.instrument.params, &b.instrument.params, |id, v| {
                out.push((AutoTarget::InstrumentParam { channel, id }, v));
            });
        }
        if a.gain != b.gain {
            out.push((AutoTarget::ChannelGain(channel), b.gain));
        }
        if a.pan != b.pan {
            out.push((AutoTarget::ChannelPan(channel), b.pan));
        }
    }
    for (track, (a, b)) in old.tracks.iter().zip(&new.tracks).enumerate() {
        if a.gain != b.gain {
            out.push((AutoTarget::TrackGain(track), b.gain));
        }
        if a.pan != b.pan {
            out.push((AutoTarget::TrackPan(track), b.pan));
        }
        for (slot, (x, y)) in a.inserts.iter().zip(&b.inserts).enumerate() {
            if let (Some(x), Some(y)) = (x, y)
                && x.kind == y.kind
            {
                params(&x.params, &y.params, |id, v| out.push((AutoTarget::EffectParam { track, slot, id }, v)));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const GAIN: AutoTarget = AutoTarget::TrackGain(2);

    fn pt(tick: u32, value: f32) -> AutoPoint {
        AutoPoint { tick, value, curve: 0.0 }
    }

    #[test]
    fn thinning_keeps_the_shape_within_the_tolerance() {
        // A ramp sampled every tick, then a hold: four points are enough.
        let mut points: Vec<AutoPoint> = (0..=100).map(|t| pt(t, t as f32 / 100.0)).collect();
        points.extend((101..=200).map(|t| pt(t, 1.0)));
        points.extend((201..=300).map(|t| pt(t, 1.0 - (t - 200) as f32 / 200.0)));
        let thin = thin(&points, 0.01);
        assert_eq!(thin.iter().map(|p| p.tick).collect::<Vec<_>>(), [0, 100, 200, 300]);
        for p in &points {
            let v = automation_value(&thin, p.tick as f64).unwrap();
            assert!((v - p.value).abs() <= 0.01 + 1e-6, "{p:?}");
        }
        // A wiggle above the tolerance stays.
        let wiggle = [pt(0, 0.0), pt(10, 0.5), pt(20, 0.0)];
        assert_eq!(thin_len(&wiggle, 0.1), 3);
        assert_eq!(thin_len(&wiggle, 0.6), 2);
    }

    fn thin_len(p: &[AutoPoint], tol: f32) -> usize {
        thin(p, tol).len()
    }

    fn project() -> Project {
        let mut p = Project::empty();
        // An existing lane: 0.2 everywhere, rising to 0.8 at the end.
        p.add_automation_point(GAIN, AutoScope::Song, 0, 0.2);
        p.add_automation_point(GAIN, AutoScope::Song, 960, 0.2);
        p.add_automation_point(GAIN, AutoScope::Song, 1920, 0.8);
        p
    }

    fn lane(p: &Project) -> Vec<(u32, f32)> {
        p.automation_lane(GAIN, AutoScope::Song).unwrap().points.iter().map(|q| (q.tick, q.value)).collect()
    }

    #[test]
    fn touch_records_while_held_then_returns() {
        let mut p = project();
        let mut r = Recorder::new(AutoMode::Touch);
        assert!(r.touch(GAIN, 100, 0.5, 0.001));
        assert!(!r.touch(GAIN, 110, 0.6, 0.001));
        // Held still: the value is kept frame after frame.
        assert!(r.tick(120, |_| Some(0.6), |_| true).is_empty());
        assert_eq!(r.open_targets().collect::<Vec<_>>(), [GAIN]);
        // Released: the lane is no longer written.
        assert_eq!(r.tick(130, |_| Some(0.6), |_| false), [GAIN]);
        assert!(r.tick(400, |_| Some(0.9), |_| false).is_empty());
        r.finish(&mut p);
        let pts = lane(&p);
        // Guard before, the gesture, a return to the old curve, the rest untouched.
        assert_eq!(pts.first(), Some(&(0, 0.2)));
        assert!(pts.contains(&(99, 0.2)));
        assert!(pts.contains(&(100, 0.5)) && pts.contains(&(110, 0.6)));
        assert!(pts.contains(&(120 + RETURN_TICKS, 0.2)), "{pts:?}");
        assert!(pts.ends_with(&[(960, 0.2), (1920, 0.8)]));
        assert!(!pts.iter().any(|&(t, v)| t > 120 && t < 168 && v != 0.6));
    }

    #[test]
    fn latch_keeps_writing_until_the_end() {
        let mut p = project();
        let mut r = Recorder::new(AutoMode::Latch);
        r.touch(GAIN, 500, 0.4, 0.001);
        for tick in (510..=1500).step_by(10) {
            assert!(r.tick(tick, |_| Some(0.4), |_| false).is_empty());
        }
        r.finish(&mut p);
        // The hold over the old points (960) is one flat stretch, thinned.
        assert_eq!(lane(&p), [(0, 0.2), (499, 0.2), (500, 0.4), (1500, 0.4), (1920, 0.8)]);
    }

    #[test]
    fn write_starts_armed_lanes_at_play_and_creates_lanes() {
        let mut p = Project::empty();
        let pan = AutoTarget::TrackPan(1);
        let mut r = Recorder::new(AutoMode::Write);
        r.start_write(&[(pan, -0.5, 0.01)], 0);
        r.tick(96, |_| Some(-0.5), |_| false);
        r.touch(pan, 192, 0.5, 0.01);
        r.tick(288, |_| Some(0.5), |_| false);
        assert_eq!(r.finish(&mut p), [pan]);
        let pts: Vec<_> = p.automation_lane(pan, AutoScope::Song).unwrap().points.iter().map(|q| (q.tick, q.value)).collect();
        assert_eq!(pts, [(0, -0.5), (96, -0.5), (192, 0.5), (288, 0.5)]);
        // Read records nothing.
        let mut read = Recorder::new(AutoMode::Read);
        assert!(!read.touch(pan, 0, 1.0, 0.01));
        assert!(read.is_empty());
    }

    #[test]
    fn a_loop_starts_a_new_stretch() {
        let mut r = Recorder::new(AutoMode::Latch);
        r.touch(GAIN, 700, 0.3, 0.001);
        r.tick(760, |_| Some(0.3), |_| false);
        r.tick(10, |_| Some(0.9), |_| false);
        let w = r.write(GAIN).unwrap();
        assert_eq!(w.segments.len(), 2);
        let mut p = project();
        r.finish(&mut p);
        let pts = lane(&p);
        assert!(pts.contains(&(10, 0.9)) && pts.contains(&(700, 0.3)) && pts.contains(&(760, 0.3)));
    }

    #[test]
    fn changed_targets_lists_value_moves_only() {
        let mut a = Project::empty();
        a.add_channel("x", crate::InstrumentDef::new("drumsynth"), 60);
        let mut b = a.clone();
        b.channels[0].instrument.set(3, 0.25);
        b.tracks[4].pan = -0.5;
        b.channels[0].gain = 0.3;
        let got = changed_targets(&a, &b);
        assert!(got.contains(&(AutoTarget::InstrumentParam { channel: 0, id: 3 }, 0.25)));
        assert!(got.contains(&(AutoTarget::TrackPan(4), -0.5)));
        assert!(got.contains(&(AutoTarget::ChannelGain(0), 0.3)));
        assert_eq!(got.len(), 3);
        b.add_channel("y", crate::InstrumentDef::new("drumsynth"), 60);
        assert!(changed_targets(&a, &b).is_empty());
    }
}
