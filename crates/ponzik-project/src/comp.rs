//! Take comping: a loop recording stacks its passes as the takes of a take
//! group, and the comp picks, region by region, which take plays (Reaper 7,
//! Logic, Cubase). The comp is a list of segments (take, start, end) in
//! ticks, sorted and never overlapping; where two segments meet they are
//! crossfaded on an equal-power curve, centered on the boundary.
//!
//! Takes keep the place where they were recorded: a segment plays its take
//! at the same point of the file as the take would there.

use ponzik_engine::{AudioClip, Sample, TempoMap};
use ponzik_i18n::tr_fmt;

use crate::{AudioClipDef, Project, SampleLoader};

/// Crossfade at comp boundaries for a new take group, in seconds.
pub const DEFAULT_CROSSFADE: f32 = 0.010;
/// Longest crossfade offered, in seconds.
pub const MAX_CROSSFADE: f32 = 0.25;

/// One pass of a loop recording.
#[derive(Clone, Debug, PartialEq)]
pub struct Take {
    pub name: String,
    /// The audio where it was recorded (its `lane`, `track`, `take` and
    /// `muted` are not used: the group has them); `color` 0: automatic.
    pub clip: AudioClipDef,
}

/// A region of the comp played by one take: `[start, end)` in ticks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompSegment {
    pub take: usize,
    pub start: u32,
    pub end: u32,
}

impl CompSegment {
    pub fn len(&self) -> u32 {
        self.end.saturating_sub(self.start)
    }

    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }

    pub fn contains(&self, tick: u32) -> bool {
        self.start <= tick && tick < self.end
    }
}

/// Which edge of a segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    Start,
    End,
}

/// Takes of one loop recording and their comp, on one playlist lane.
#[derive(Clone, Debug, PartialEq)]
pub struct TakeGroup {
    /// Playlist lane of the comp; unfolded, the takes show under it.
    pub lane: usize,
    /// Mixer track.
    pub track: usize,
    /// In recording order (the newest last; drawn on top).
    pub takes: Vec<Take>,
    /// Sorted, non-overlapping, each inside its take.
    pub comp: Vec<CompSegment>,
    /// Crossfade at the boundaries between segments, in seconds.
    pub crossfade: f32,
    /// Take lanes hidden.
    pub folded: bool,
    /// Silent (after flattening, the takes are kept until deleted).
    pub muted: bool,
    /// Take heard alone instead of the comp (audition, not saved).
    pub solo: Option<usize>,
}

/// A segment ready to play: the clip, and its crossfades in seconds.
#[derive(Clone, Debug, PartialEq)]
pub struct CompPiece {
    pub clip: AudioClipDef,
    pub xfade_in: f32,
    pub xfade_out: f32,
}

/// A flattened comp: its audio and the clip that plays it (path to fill).
#[derive(Clone, Debug)]
pub struct Flattened {
    pub sample: Sample,
    pub clip: AudioClipDef,
}

impl TakeGroup {
    /// A group of these takes, the comp empty.
    pub fn new(lane: usize, track: usize, takes: Vec<Take>) -> Self {
        Self { lane, track, takes, comp: Vec::new(), crossfade: DEFAULT_CROSSFADE, folded: false, muted: false, solo: None }
    }

    /// Ticks covered by a take.
    pub fn take_span(&self, take: usize) -> Option<(u32, u32)> {
        self.takes.get(take).map(|t| (t.clip.start, t.clip.start.saturating_add(t.clip.len)))
    }

    /// Ticks covered by all the takes.
    pub fn span(&self) -> (u32, u32) {
        let a = self.takes.iter().map(|t| t.clip.start).min().unwrap_or(0);
        let b = self.takes.iter().map(|t| t.clip.start.saturating_add(t.clip.len)).max().unwrap_or(a);
        (a, b)
    }

    /// Ticks covered by the comp.
    pub fn comp_span(&self) -> Option<(u32, u32)> {
        Some((self.comp.first()?.start, self.comp.last()?.end))
    }

    /// The segment playing at `tick`.
    pub fn segment_at(&self, tick: u32) -> Option<usize> {
        self.comp.iter().position(|s| s.contains(tick))
    }

    /// Sorts the segments, drops the empty ones, the ones of missing takes
    /// and what reaches outside a take or over an earlier segment.
    pub fn normalize(&mut self) {
        let spans: Vec<(u32, u32)> = (0..self.takes.len()).filter_map(|t| self.take_span(t)).collect();
        let mut comp: Vec<CompSegment> = self
            .comp
            .iter()
            .filter_map(|s| {
                let (a, b) = *spans.get(s.take)?;
                let seg = CompSegment { take: s.take, start: s.start.max(a), end: s.end.min(b) };
                (!seg.is_empty()).then_some(seg)
            })
            .collect();
        comp.sort_by_key(|s| (s.start, s.end));
        let mut out: Vec<CompSegment> = Vec::with_capacity(comp.len());
        for mut s in comp {
            if let Some(prev) = out.last() {
                s.start = s.start.max(prev.end);
            }
            if !s.is_empty() {
                out.push(s);
            }
        }
        self.comp = out;
        if self.solo.is_some_and(|t| t >= self.takes.len()) {
            self.solo = None;
        }
    }

    /// Swipe: `take` plays `[a, b)` (kept inside the take). What the comp had
    /// there is cut away; touching segments of the same take merge.
    /// Returns false when nothing changed.
    pub fn swipe(&mut self, take: usize, a: u32, b: u32) -> bool {
        let Some((ta, tb)) = self.take_span(take) else { return false };
        let (a, b) = (a.min(b).max(ta), a.max(b).min(tb));
        if a >= b {
            return false;
        }
        let before = self.comp.clone();
        let mut comp = Vec::with_capacity(self.comp.len() + 2);
        for s in &self.comp {
            if s.end <= a || s.start >= b {
                comp.push(*s);
                continue;
            }
            if s.start < a {
                comp.push(CompSegment { end: a, ..*s });
            }
            if s.end > b {
                comp.push(CompSegment { start: b, ..*s });
            }
        }
        let at = comp.partition_point(|s| s.start < a);
        comp.insert(at, CompSegment { take, start: a, end: b });
        // The new segment absorbs touching neighbors of its take.
        let mut at = at;
        if at > 0 && comp[at - 1].take == take && comp[at - 1].end == a {
            comp[at - 1].end = comp[at].end;
            comp.remove(at);
            at -= 1;
        }
        if comp.get(at + 1).is_some_and(|n| n.take == take && n.start == b) {
            comp[at].end = comp[at + 1].end;
            comp.remove(at + 1);
        }
        self.comp = comp;
        self.comp != before
    }

    /// The whole take plays, alone (quick swipe off: a click on a take).
    pub fn choose_whole(&mut self, take: usize) -> bool {
        let Some((a, b)) = self.take_span(take) else { return false };
        let comp = vec![CompSegment { take, start: a, end: b }];
        let changed = self.comp != comp;
        self.comp = comp;
        changed
    }

    /// Up/down on the segment at `tick`: the previous (`step` < 0) or next
    /// take that covers some of it plays it. Returns the new take.
    pub fn switch_take_at(&mut self, tick: u32, step: i32) -> Option<usize> {
        let i = self.segment_at(tick)?;
        let seg = self.comp[i];
        let n = self.takes.len() as i32;
        let covers = |t: usize| self.take_span(t).is_some_and(|(a, b)| a < seg.end && b > seg.start);
        let take = (1..n).map(|k| (seg.take as i32 + k * step.signum()).rem_euclid(n) as usize).find(|&t| covers(t))?;
        self.swipe(take, seg.start, seg.end);
        Some(take)
    }

    /// Cuts the segment at `tick` in two (same take). False if `tick` is on
    /// an edge or outside the comp.
    pub fn split_at(&mut self, tick: u32) -> bool {
        let Some(i) = self.segment_at(tick).filter(|&i| self.comp[i].start < tick) else { return false };
        let seg = self.comp[i];
        self.comp[i].end = tick;
        self.comp.insert(i + 1, CompSegment { start: tick, ..seg });
        true
    }

    /// Range an edge of a segment can move in: inside its take, at least a
    /// tick long, and (a boundary moving both segments) inside the touching
    /// neighbor and its take, or up to a free neighbor.
    pub fn edge_range(&self, seg: usize, edge: Edge) -> Option<(u32, u32)> {
        let s = *self.comp.get(seg)?;
        let (ta, tb) = self.take_span(s.take)?;
        Some(match edge {
            Edge::End => {
                let next = self.comp.get(seg + 1);
                let hi = match next {
                    Some(n) if n.start == s.end => {
                        let (na, _) = self.take_span(n.take)?;
                        return Some(((s.start + 1).max(na), tb.min(n.end - 1)));
                    }
                    Some(n) => n.start,
                    None => u32::MAX,
                };
                (s.start + 1, tb.min(hi))
            }
            Edge::Start => {
                let prev = seg.checked_sub(1).and_then(|p| self.comp.get(p));
                let lo = match prev {
                    Some(p) if p.end == s.start => {
                        let (_, pb) = self.take_span(p.take)?;
                        return Some((ta.max(p.start + 1), (s.end - 1).min(pb)));
                    }
                    Some(p) => p.end,
                    None => 0,
                };
                (ta.max(lo), s.end - 1)
            }
        })
    }

    /// Drags an edge of a segment to `tick` (kept in [`TakeGroup::edge_range`]);
    /// a boundary between two touching segments moves both. Returns where it went.
    pub fn move_edge(&mut self, seg: usize, edge: Edge, tick: u32) -> Option<u32> {
        let (lo, hi) = self.edge_range(seg, edge)?;
        if lo > hi {
            return None;
        }
        let t = tick.clamp(lo, hi);
        let s = self.comp[seg];
        match edge {
            Edge::End => {
                if let Some(n) = self.comp.get_mut(seg + 1).filter(|n| n.start == s.end) {
                    n.start = t;
                }
                self.comp[seg].end = t;
            }
            Edge::Start => {
                if let Some(p) = seg.checked_sub(1).and_then(|p| self.comp.get_mut(p)).filter(|p| p.end == s.start) {
                    p.end = t;
                }
                self.comp[seg].start = t;
            }
        }
        Some(t)
    }

    /// Takes that play no segment of the comp.
    pub fn unused_takes(&self) -> Vec<usize> {
        (0..self.takes.len()).filter(|&t| !self.comp.iter().any(|s| s.take == t)).collect()
    }

    /// Removes a take and its segments.
    pub fn delete_take(&mut self, take: usize) {
        if take >= self.takes.len() {
            return;
        }
        self.takes.remove(take);
        self.comp.retain(|s| s.take != take);
        for s in &mut self.comp {
            if s.take > take {
                s.take -= 1;
            }
        }
        self.solo = match self.solo {
            Some(t) if t == take => None,
            Some(t) if t > take => Some(t - 1),
            other => other,
        };
    }

    /// Removes the takes the comp doesn't use; returns how many went.
    pub fn delete_unused_takes(&mut self) -> usize {
        let unused = self.unused_takes();
        for &t in unused.iter().rev() {
            self.delete_take(t);
        }
        unused.len()
    }

    /// Moves the whole group in time.
    pub fn shift(&mut self, delta: i64) {
        let at = |t: u32| (t as i64 + delta).clamp(0, u32::MAX as i64) as u32;
        for t in &mut self.takes {
            t.clip.start = at(t.clip.start);
        }
        for s in &mut self.comp {
            s.start = at(s.start);
            s.end = at(s.end);
        }
    }

    /// Crossfade at the boundary between segments `i` and `i + 1`, if they
    /// touch: never longer than either of them, so windows don't cross.
    fn boundary_fade(&self, i: usize, map: &TempoMap) -> f32 {
        let (Some(a), Some(b)) = (self.comp.get(i), self.comp.get(i + 1)) else { return 0.0 };
        if a.end != b.start || self.crossfade <= 0.0 {
            return 0.0;
        }
        let secs = |s: &CompSegment| (map.secs_at(s.end as f64) - map.secs_at(s.start as f64)) as f32;
        self.crossfade.min(MAX_CROSSFADE).min(secs(a)).min(secs(b))
    }

    /// The comp as clips to play (sound of a segment: its take there).
    pub fn comp_pieces(&self, map: &TempoMap) -> Vec<CompPiece> {
        self.comp
            .iter()
            .enumerate()
            .filter_map(|(i, s)| {
                let take = &self.takes.get(s.take)?.clip;
                let clip = AudioClipDef {
                    start: s.start,
                    len: s.len(),
                    offset: take.offset_at(s.start as f64, map),
                    track: self.track,
                    lane: self.lane,
                    ..take.clone()
                };
                let xfade_in = i.checked_sub(1).map_or(0.0, |p| self.boundary_fade(p, map));
                Some(CompPiece { clip, xfade_in, xfade_out: self.boundary_fade(i, map) })
            })
            .collect()
    }

    /// What plays: the comp, or the take auditioned alone, or nothing (muted).
    pub fn pieces(&self, map: &TempoMap) -> Vec<CompPiece> {
        if self.muted {
            return Vec::new();
        }
        if let Some(t) = self.solo.and_then(|t| self.takes.get(t)) {
            let clip = AudioClipDef { track: self.track, lane: self.lane, ..t.clip.clone() };
            return vec![CompPiece { clip, xfade_in: 0.0, xfade_out: 0.0 }];
        }
        self.comp_pieces(map)
    }

    /// Renders the comp to one piece of audio at `sample_rate`, exactly as it
    /// plays: the returned clip (on the group's lane and track) plays it back
    /// from the comp start, with the outer fades of the first and last takes.
    /// `None`: empty comp or a take whose file doesn't load.
    pub fn flatten(&self, map: &TempoMap, sample_rate: f32, loader: SampleLoader) -> Option<Flattened> {
        let (a, b) = self.comp_span()?;
        let pieces = self.comp_pieces(map);
        let last = pieces.len().checked_sub(1)?;
        let mut clips = Vec::with_capacity(pieces.len());
        for (i, p) in pieces.iter().enumerate() {
            let mut clip = engine_clip(&p.clip, loader(&p.clip.path)?, None);
            (clip.xfade_in, clip.xfade_out) = (p.xfade_in, p.xfade_out);
            // The outer fades go to the flattened clip.
            if i == 0 {
                clip.fade_in = -1.0;
            }
            if i == last {
                clip.fade_out = -1.0;
            }
            clips.push(clip);
        }
        let from = map.secs_at(a as f64);
        let frames = ((map.secs_at(b as f64) - from) * sample_rate as f64).ceil() as usize + 1;
        let (left, right) = ponzik_engine::render_audio_clips(&clips, map, sample_rate, from, frames);
        let (first, end) = (&pieces[0].clip, &pieces[last].clip);
        let clip = AudioClipDef {
            path: String::new(),
            lane: self.lane,
            track: self.track,
            start: a,
            len: b - a,
            offset: 0.0,
            gain: 1.0,
            fade_in: first.fade_in,
            fade_in_curve: first.fade_in_curve,
            fade_out: end.fade_out,
            fade_out_curve: end.fade_out_curve,
            ..Default::default()
        };
        Some(Flattened { sample: Sample { name: String::new(), sample_rate, left, right, markers: Vec::new() }, clip })
    }
}

/// The engine clip for a clip of the project (warp: its map and render).
pub(crate) fn engine_clip(c: &AudioClipDef, sample: std::sync::Arc<Sample>, warp: Option<ponzik_engine::ClipWarp>) -> AudioClip {
    AudioClip {
        sample,
        fade_in_curve: c.fade_in_curve,
        fade_out_curve: c.fade_out_curve,
        reverse: c.reverse,
        looped: c.looped,
        warp,
        track: c.track,
        start: c.start,
        len: c.len,
        offset: c.offset,
        gain: c.gain,
        fade_in: c.fade_in,
        fade_out: c.fade_out,
        xfade_in: 0.0,
        xfade_out: 0.0,
        cycle: 0,
        phase: 0,
    }
}

impl Project {
    /// Older projects kept the passes of a loop recording as audio clips of
    /// one take group (`take` > 0), one lane each, all muted but the chosen
    /// ones: each such group becomes a [`TakeGroup`] whose comp is what played.
    pub fn convert_take_clips(&mut self) {
        let mut ids: Vec<u32> = self.audio_clips.iter().map(|c| c.take).filter(|&t| t > 0).collect();
        ids.sort_unstable();
        ids.dedup();
        for id in ids {
            let mut clips: Vec<AudioClipDef> = self.audio_clips.iter().filter(|c| c.take == id).cloned().collect();
            if clips.len() < 2 {
                continue;
            }
            self.audio_clips.retain(|c| c.take != id);
            clips.sort_by_key(|c| (c.lane, c.start));
            let (lane, track) = (clips[0].lane, clips[0].track);
            let playing: Vec<bool> = clips.iter().map(|c| !c.muted).collect();
            let takes = clips
                .into_iter()
                .enumerate()
                .map(|(k, c)| Take { name: tr_fmt!("Take {}", k + 1), clip: AudioClipDef { take: 0, muted: false, lane: 0, ..c } })
                .collect();
            let mut group = TakeGroup::new(lane, track, takes);
            for (k, _) in playing.iter().enumerate().filter(|(_, p)| **p) {
                if let Some((a, b)) = group.take_span(k) {
                    group.swipe(k, a, b);
                }
            }
            self.take_groups.push(group);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn take(start: u32, len: u32) -> Take {
        Take { name: "t".into(), clip: AudioClipDef { path: "a.wav".into(), start, len, offset: start as f64 / 192.0, ..Default::default() } }
    }

    fn group() -> TakeGroup {
        // Three passes of bars 1–2 (0..768), the last one shorter.
        TakeGroup::new(2, 3, vec![take(0, 768), take(0, 768), take(0, 600)])
    }

    fn segs(g: &TakeGroup) -> Vec<(usize, u32, u32)> {
        g.comp.iter().map(|s| (s.take, s.start, s.end)).collect()
    }

    #[test]
    fn swipes_cut_overlaps_and_merge_with_their_take() {
        let mut g = group();
        assert!(g.swipe(0, 0, 768));
        assert!(g.swipe(1, 200, 400));
        assert_eq!(segs(&g), [(0, 0, 200), (1, 200, 400), (0, 400, 768)]);
        // Over a boundary: the new segment eats into both neighbors.
        assert!(g.swipe(2, 300, 500));
        assert_eq!(segs(&g), [(0, 0, 200), (1, 200, 300), (2, 300, 500), (0, 500, 768)]);
        // Same take touching: merged.
        assert!(g.swipe(1, 300, 350));
        assert_eq!(segs(&g), [(0, 0, 200), (1, 200, 350), (2, 350, 500), (0, 500, 768)]);
        // A whole segment covered: gone.
        assert!(g.swipe(0, 100, 400));
        assert_eq!(segs(&g), [(0, 0, 400), (2, 400, 500), (0, 500, 768)]);
        // Kept inside the take (take 2 ends at 600), drawn backwards too.
        assert!(g.swipe(2, 700, 450));
        assert_eq!(segs(&g), [(0, 0, 400), (2, 400, 600), (0, 600, 768)]);
        assert!(!g.swipe(2, 650, 700), "outside the take");
        assert!(!g.swipe(2, 400, 600), "no change");
        assert!(!g.swipe(9, 0, 10), "no such take");
    }

    #[test]
    fn swipes_leave_gaps_and_keep_other_boundaries() {
        let mut g = group();
        g.swipe(0, 0, 100);
        g.swipe(0, 200, 300);
        assert_eq!(segs(&g), [(0, 0, 100), (0, 200, 300)]);
        // Filling the gap merges all three.
        g.swipe(0, 100, 200);
        assert_eq!(segs(&g), [(0, 0, 300)]);
        // A split boundary stays until a swipe of that take covers it.
        assert!(g.split_at(150));
        assert_eq!(segs(&g), [(0, 0, 150), (0, 150, 300)]);
        assert!(!g.split_at(150), "on an edge");
        assert!(!g.split_at(500), "outside");
        g.swipe(1, 150, 300);
        assert_eq!(segs(&g), [(0, 0, 150), (1, 150, 300)]);
    }

    #[test]
    fn up_and_down_switch_the_take_of_a_segment() {
        let mut g = group();
        g.swipe(0, 0, 768);
        g.split_at(384);
        assert_eq!(g.switch_take_at(500, 1), Some(1));
        assert_eq!(segs(&g), [(0, 0, 384), (1, 384, 768)]);
        // Take 2 stops at 600: it plays what it covers.
        assert_eq!(g.switch_take_at(500, 1), Some(2));
        assert_eq!(segs(&g), [(0, 0, 384), (2, 384, 600), (1, 600, 768)]);
        // Down from take 0 wraps to the newest.
        assert_eq!(g.switch_take_at(10, -1), Some(2));
        assert_eq!(segs(&g), [(2, 0, 600), (1, 600, 768)], "merged with the same take");
        assert_eq!(g.switch_take_at(700, 1), Some(0), "take 2 does not reach 700");
        assert_eq!(g.switch_take_at(5_000, 1), None);
    }

    #[test]
    fn edges_move_boundaries_inside_their_takes() {
        let mut g = group();
        g.swipe(0, 0, 768);
        g.swipe(2, 200, 400);
        assert_eq!(segs(&g), [(0, 0, 200), (2, 200, 400), (0, 400, 768)]);
        // The boundary moves both segments.
        assert_eq!(g.move_edge(0, Edge::End, 250), Some(250));
        assert_eq!(segs(&g), [(0, 0, 250), (2, 250, 400), (0, 400, 768)]);
        // Never past the neighbor (a tick left) nor outside take 2 (ends at 600).
        assert_eq!(g.move_edge(1, Edge::End, 700), Some(600));
        assert_eq!(g.move_edge(1, Edge::Start, 900), Some(599));
        assert_eq!(segs(&g), [(0, 0, 599), (2, 599, 600), (0, 600, 768)]);
        // Outer edges: inside the take.
        assert_eq!(g.move_edge(2, Edge::End, 10_000), Some(768));
        let mut h = group();
        h.swipe(1, 100, 200);
        assert_eq!(h.move_edge(0, Edge::Start, 0), Some(0));
        assert_eq!(h.edge_range(0, Edge::End), Some((1, 768)));
    }

    #[test]
    fn unused_takes_go_and_segments_follow_the_new_indices() {
        let mut g = group();
        g.swipe(2, 0, 300);
        g.swipe(1, 300, 768);
        g.solo = Some(2);
        assert_eq!(g.unused_takes(), [0]);
        assert_eq!(g.delete_unused_takes(), 1);
        assert_eq!(segs(&g), [(1, 0, 300), (0, 300, 768)]);
        assert_eq!(g.solo, Some(1));
        g.delete_take(1);
        assert_eq!(segs(&g), [(0, 300, 768)]);
        assert_eq!(g.solo, None);
    }

    #[test]
    fn normalize_repairs_loaded_segments() {
        let mut g = group();
        g.comp = vec![
            CompSegment { take: 1, start: 300, end: 500 },
            CompSegment { take: 0, start: 0, end: 400 },
            CompSegment { take: 7, start: 0, end: 10 },
            CompSegment { take: 2, start: 550, end: 900 },
            CompSegment { take: 0, start: 20, end: 20 },
        ];
        g.normalize();
        assert_eq!(segs(&g), [(0, 0, 400), (1, 400, 500), (2, 550, 600)]);
    }

    #[test]
    fn pieces_crossfade_only_touching_segments() {
        let map = TempoMap::constant(120.0);
        let mut g = group();
        g.swipe(0, 0, 192);
        g.swipe(1, 192, 384);
        g.swipe(2, 480, 600);
        let p = g.pieces(&map);
        assert_eq!(p.len(), 3);
        assert_eq!((p[0].xfade_in, p[0].xfade_out), (0.0, DEFAULT_CROSSFADE));
        assert_eq!((p[1].xfade_in, p[1].xfade_out), (DEFAULT_CROSSFADE, 0.0));
        assert_eq!((p[2].xfade_in, p[2].xfade_out), (0.0, 0.0));
        // A segment plays its take at the same point of the file.
        assert!((p[1].clip.offset - 1.0).abs() < 1e-9);
        assert_eq!((p[1].clip.start, p[1].clip.len, p[1].clip.track), (192, 192, 3));
        // A crossfade never outlasts a segment (2 ticks: about 10 ms at 120 BPM).
        g.crossfade = 0.2;
        g.swipe(2, 190, 192);
        let p = g.pieces(&map);
        assert!((p[0].xfade_out - 2.0 / 192.0).abs() < 1e-6);
        g.solo = Some(1);
        assert_eq!(g.pieces(&map).len(), 1);
        g.muted = true;
        assert!(g.pieces(&map).is_empty());
    }

    /// Flattening renders what plays: the song with the comp and the song
    /// with the flattened clip in its place render the same.
    #[test]
    fn flatten_output_equals_playback() {
        let sr = 48_000.0f32;
        let len = 3 * 48_000;
        let wave = |f: f32, k: usize| (k as f32 / sr * f * std::f32::consts::TAU).sin() * 0.6;
        // One file with three passes of 1 s (192 ticks at 120 BPM), as a loop recording.
        let left: Vec<f32> = (0..len).map(|k| wave(220.0 + 110.0 * (k / 48_000) as f32, k)).collect();
        let right: Vec<f32> = left.iter().map(|s| s * 0.5).collect();
        let file = Arc::new(Sample { name: "takes".into(), sample_rate: 44_100.0, left, right, markers: Vec::new() });
        let mut p = Project::empty();
        p.bpm = 120.0;
        let pass = |k: usize| Take {
            name: format!("Take {k}"),
            clip: AudioClipDef { path: "takes.wav".into(), start: 192, len: 192, offset: k as f64, fade_in: 0.003, fade_out: 0.004, gain: 0.8, ..Default::default() },
        };
        let mut g = TakeGroup::new(0, 2, vec![pass(0), pass(1), pass(2)]);
        g.swipe(0, 192, 384);
        g.swipe(2, 240, 300);
        g.swipe(1, 330, 384);
        g.crossfade = 0.02;
        p.take_groups.push(g.clone());
        let flat_file = std::cell::RefCell::new(None);
        let loader = |path: &str| match path {
            "takes.wav" => Some(file.clone()),
            "flat.wav" => flat_file.borrow().clone(),
            _ => None,
        };
        let flat = g.flatten(&p.tempo_map(), sr, &loader).expect("flattened");
        assert_eq!((flat.clip.start, flat.clip.len, flat.clip.track), (192, 192, 2));
        *flat_file.borrow_mut() = Some(Arc::new(flat.sample));
        let comp = crate::render_song(&p, sr as u32, &loader, crate::RenderMode::Song);
        let mut q = p.clone();
        q.take_groups.clear();
        q.audio_clips.push(AudioClipDef { path: "flat.wav".into(), ..flat.clip });
        let flattened = crate::render_song(&q, sr as u32, &loader, crate::RenderMode::Song);
        assert_eq!(comp.len(), flattened.len());
        let peak = comp.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak > 0.1, "the comp plays");
        for (k, (a, b)) in comp.iter().zip(&flattened).enumerate() {
            assert!((a - b).abs() < 1e-4, "{k}: {a} vs {b}");
        }
    }

    #[test]
    fn time_edits_move_copy_and_remove_whole_groups() {
        let mut p = Project::empty();
        let mut g = TakeGroup::new(0, 2, vec![take(768, 384), take(768, 384)]);
        g.swipe(1, 768, 1152);
        p.take_groups.push(g);
        // A bar removed before it: the group moves left, comp included.
        p.delete_range(0, 384);
        assert_eq!(p.take_groups[0].span(), (384, 768));
        assert_eq!(segs(&p.take_groups[0]), [(1, 384, 768)]);
        assert_eq!(p.song_len(), 768);
        // Duplicated with its range.
        p.duplicate_time(384, 768);
        assert_eq!(p.take_groups.len(), 2);
        assert_eq!(segs(&p.take_groups[1]), [(1, 768, 1152)]);
        // Cleared whole; a range across it leaves it alone.
        p.clear_time(700, 800);
        assert_eq!(p.take_groups.len(), 2);
        p.clear_time(768, 1152);
        assert_eq!(p.take_groups.len(), 1);
        assert!(p.track_in_use(2));
    }

    #[test]
    fn old_take_clips_become_a_take_group() {
        let mut p = Project::empty();
        let clip = |lane: usize, offset: f64, muted: bool, take: u32| AudioClipDef {
            path: "rec.wav".into(),
            lane,
            track: 4,
            start: 768,
            len: 384,
            offset,
            take,
            muted,
            ..Default::default()
        };
        p.audio_clips.push(AudioClipDef { path: "other.wav".into(), ..Default::default() });
        p.audio_clips.push(clip(6, 4.0, true, 2));
        p.audio_clips.push(clip(5, 2.0, false, 2));
        p.audio_clips.push(clip(7, 6.0, true, 2));
        // A lone clip of a group stays a clip.
        p.audio_clips.push(clip(9, 0.0, false, 3));
        p.convert_take_clips();
        assert_eq!(p.audio_clips.len(), 2);
        assert_eq!(p.audio_clips[1].take, 3);
        assert_eq!(p.take_groups.len(), 1);
        let g = &p.take_groups[0];
        assert_eq!((g.lane, g.track, g.takes.len()), (5, 4, 3));
        let offsets: Vec<f64> = g.takes.iter().map(|t| t.clip.offset).collect();
        assert_eq!(offsets, [2.0, 4.0, 6.0]);
        assert_eq!(segs(g), [(0, 768, 1152)]);
        assert!(g.takes.iter().all(|t| !t.clip.muted && t.clip.take == 0));
    }
}
