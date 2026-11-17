//! Arrangement: tempo and time signature changes, section markers, and the
//! timeline operations behind section moves (cut a time range out, open a
//! gap, paste a range back) on everything that lives on the song timeline:
//! pattern clips, audio clips, song automation, tempo and signature changes
//! and the markers themselves.

use ponzik_engine::{AutoPoint, AutoScope, AutoTarget, TempoPoint, TimeSig};

use crate::{AudioClipDef, ClipDef, Marker, Project};

/// Colors offered for markers, in order (a new marker takes the next one).
pub const MARKER_COLORS: [u32; 8] = [0x3fd45a, 0x3ccfc4, 0xf0b44a, 0xe878b4, 0x2fb6e0, 0xa98cf0, 0xf08a5d, 0xc7e85a];

/// Everything in a time range, with ticks relative to its start.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TimeSlice {
    pub len: u32,
    pub clips: Vec<ClipDef>,
    pub audio: Vec<AudioClipDef>,
    /// Song automation lanes: points relative to the start, with the value at
    /// the start and just before the end pinned.
    pub lanes: Vec<(AutoTarget, Vec<AutoPoint>)>,
    /// Tempo at the start (tick 0) and the changes inside.
    pub tempos: Vec<TempoPoint>,
    /// Signature at the start (tick 0) and the changes inside.
    pub sigs: Vec<TimeSig>,
    pub markers: Vec<Marker>,
}

/// How a moved or duplicated section lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    /// Opens a gap: what was at the destination moves right.
    Insert,
    /// Replaces what was at the destination.
    Overwrite,
}

/// Inserts a point holding the curve's current value at `tick` (none there yet).
fn pin(points: &mut Vec<AutoPoint>, tick: u32) {
    let Some(value) = ponzik_engine::sequence::automation_value(points, tick as f64) else { return };
    let at = points.partition_point(|p| p.tick < tick);
    if points.get(at).is_none_or(|p| p.tick != tick) {
        points.insert(at, AutoPoint { tick, value, curve: 0.0 });
    }
}

fn shifted(tick: u32, delta: i64) -> u32 {
    (tick as i64 + delta).clamp(0, u32::MAX as i64) as u32
}

impl Project {
    // --- Tempo and signatures ---

    /// Adds (or replaces) a tempo change; at tick 0 it sets `bpm`. Returns the
    /// index of the point in the tempo lane (0 = the first tempo).
    pub fn set_tempo_change(&mut self, tick: u32, bpm: f32, ramp: bool) -> usize {
        let bpm = bpm.clamp(ponzik_engine::tempo_map::MIN_BPM as f32, ponzik_engine::tempo_map::MAX_BPM as f32);
        if tick == 0 {
            self.bpm = bpm;
            return 0;
        }
        let at = self.tempo_changes.partition_point(|p| p.tick < tick);
        match self.tempo_changes.get_mut(at) {
            Some(p) if p.tick == tick => *p = TempoPoint { tick, bpm, ramp },
            _ => self.tempo_changes.insert(at, TempoPoint { tick, bpm, ramp }),
        }
        at + 1
    }

    /// Index in the tempo lane (0 = the first tempo) of the point that sets
    /// the tempo at `tick`.
    pub fn tempo_point_at(&self, tick: f64) -> usize {
        self.tempo_changes.partition_point(|p| p.tick as f64 <= tick)
    }

    /// Tempo of the tempo lane point `i` (0 = the first tempo).
    pub fn tempo_point_bpm(&self, i: usize) -> f32 {
        match i.checked_sub(1).and_then(|j| self.tempo_changes.get(j)) {
            Some(p) => p.bpm,
            None => self.bpm,
        }
    }

    /// Sets the tempo of lane point `i` (0 = the first tempo).
    pub fn set_tempo_point_bpm(&mut self, i: usize, bpm: f32) {
        let bpm = bpm.clamp(ponzik_engine::tempo_map::MIN_BPM as f32, ponzik_engine::tempo_map::MAX_BPM as f32);
        match i.checked_sub(1).and_then(|j| self.tempo_changes.get_mut(j)) {
            Some(p) => p.bpm = bpm,
            None => self.bpm = bpm,
        }
    }

    /// Adds (or replaces) a time signature starting at `tick`. Returns its index.
    pub fn set_signature(&mut self, tick: u32, num: u8, den: u8) -> usize {
        let sig = TimeSig { tick, num: num.clamp(1, 32), den: if TimeSig::valid_den(den) { den } else { 4 } };
        let at = self.signatures.partition_point(|s| s.tick < tick);
        match self.signatures.get_mut(at) {
            Some(s) if s.tick == tick => *s = sig,
            _ => self.signatures.insert(at, sig),
        }
        at
    }

    /// Drops tempo and signature changes that change nothing.
    pub fn normalize_tempo(&mut self) {
        self.tempo_changes.retain(|p| p.tick > 0);
        self.tempo_changes.sort_by_key(|p| p.tick);
        self.tempo_changes.dedup_by(|later, earlier| {
            let same = later.tick == earlier.tick;
            if same {
                *earlier = *later;
            }
            same
        });
        let mut kept: Vec<TempoPoint> = Vec::with_capacity(self.tempo_changes.len());
        for i in 0..self.tempo_changes.len() {
            let p = self.tempo_changes[i];
            let prev = kept.last().map_or(self.bpm, |q| q.bpm);
            let next_ramps = self.tempo_changes.get(i + 1).is_some_and(|q| q.ramp);
            if p.bpm == prev && !next_ramps {
                continue;
            }
            kept.push(p);
        }
        self.tempo_changes = kept;

        self.signatures.sort_by_key(|s| s.tick);
        self.signatures.dedup_by(|later, earlier| {
            let same = later.tick == earlier.tick;
            if same {
                *earlier = *later;
            }
            same
        });
        let mut kept: Vec<TimeSig> = Vec::with_capacity(self.signatures.len());
        for &s in &self.signatures {
            let prev = kept.last().copied().unwrap_or(TimeSig::FOUR_FOUR);
            // Same meter on the bar grid: the change doesn't move a single bar line.
            if s.same_meter(prev) && (s.tick - prev.tick) % prev.bar_len() == 0 {
                continue;
            }
            kept.push(s);
        }
        self.signatures = kept;
    }

    // --- Markers and sections ---

    /// Adds a marker at `tick` and returns its index.
    pub fn add_marker(&mut self, tick: u32, name: impl Into<String>) -> usize {
        let color = MARKER_COLORS[self.markers.len() % MARKER_COLORS.len()];
        let at = self.markers.partition_point(|m| m.tick <= tick);
        self.markers.insert(at, Marker { tick, name: name.into(), color });
        at
    }

    /// Re-sorts the markers after a move; returns the new index of `index`.
    pub fn sort_markers(&mut self, index: usize) -> usize {
        let Some(moved) = self.markers.get(index).cloned() else { return index };
        self.markers.remove(index);
        let at = self.markers.partition_point(|m| m.tick <= moved.tick);
        self.markers.insert(at, moved);
        at
    }

    /// Range `[start, end)` of section `i`: from its marker to the next one,
    /// the last one to the end of the song (one bar at least).
    pub fn section(&self, i: usize) -> Option<(u32, u32)> {
        let start = self.markers.get(i)?.tick;
        let end = match self.markers.get(i + 1) {
            Some(m) => m.tick,
            None => {
                let map = self.tempo_map();
                let bar_end = map.ceil_bar(start as f64 + 1.0);
                self.song_len().max(bar_end)
            }
        };
        Some((start, end.max(start + 1)))
    }

    /// Ranges of every section, in marker order.
    pub fn sections(&self) -> Vec<(u32, u32)> {
        (0..self.markers.len()).filter_map(|i| self.section(i)).collect()
    }

    // --- Timeline operations ---

    /// Copy of everything in `[a, b)`, cut at the edges.
    pub fn extract(&self, a: u32, b: u32) -> TimeSlice {
        let b = b.max(a);
        let cut = |start: u32, len: u32| {
            let (s, e) = (start.max(a), start.saturating_add(len).min(b));
            (s < e).then(|| (s, e - s, s - start))
        };
        let clips = self
            .clips
            .iter()
            .filter_map(|c| {
                let (s, len, skip) = cut(c.start, c.len)?;
                Some(ClipDef { start: s - a, len, offset: c.offset + skip, ..*c })
            })
            .collect();
        let map = self.tempo_map();
        let audio = self
            .audio_clips
            .iter()
            .filter_map(|c| {
                let (s, len, skip) = cut(c.start, c.len)?;
                let fade_in = if skip == 0 { c.fade_in } else { 0.0 };
                let fade_out = if s + len == c.start + c.len { c.fade_out } else { 0.0 };
                let offset = c.offset_at(s as f64, &map);
                Some(AudioClipDef { start: s - a, len, offset, fade_in, fade_out, ..c.clone() })
            })
            .collect();
        let len = b - a;
        let lanes = self
            .automation
            .iter()
            .filter(|l| l.scope == AutoScope::Song && !l.points.is_empty())
            .map(|l| {
                let at = |t: u32| l.value_at(t as f64).unwrap_or(0.0);
                let mut pts: Vec<AutoPoint> =
                    l.points.iter().filter(|p| p.tick >= a && p.tick < b).map(|p| AutoPoint { tick: p.tick - a, ..*p }).collect();
                if pts.first().is_none_or(|p| p.tick != 0) {
                    pts.insert(0, AutoPoint { tick: 0, value: at(a), curve: 0.0 });
                }
                if len > 1 && pts.last().is_some_and(|p| p.tick < len - 1) {
                    pts.push(AutoPoint { tick: len - 1, value: at(b - 1), curve: 0.0 });
                }
                (l.target, pts)
            })
            .collect();
        let lane_map = self.tempo_lane_map();
        let mut tempos = vec![TempoPoint { tick: 0, bpm: lane_map.bpm_at(a as f64) as f32, ramp: false }];
        tempos.extend(self.tempo_changes.iter().filter(|p| p.tick > a && p.tick < b).map(|p| TempoPoint { tick: p.tick - a, ..*p }));
        let mut sigs = vec![TimeSig { tick: 0, ..lane_map.sig_at(a as f64) }];
        sigs.extend(self.signatures.iter().filter(|s| s.tick > a && s.tick < b).map(|s| TimeSig { tick: s.tick - a, ..*s }));
        let markers = self
            .markers
            .iter()
            .filter(|m| m.tick >= a && m.tick < b)
            .map(|m| Marker { tick: m.tick - a, ..m.clone() })
            .collect();
        TimeSlice { len, clips, audio, lanes, tempos, sigs, markers }
    }

    /// Empties `[a, b)`: clips are cut, automation points, tempo and signature
    /// changes and markers inside go away; what follows keeps its values.
    pub fn clear_range(&mut self, a: u32, b: u32) {
        if b <= a {
            return;
        }
        let lane_map = self.tempo_lane_map();
        self.clear_time(a, b);
        for lane in self.automation.iter_mut().filter(|l| l.scope == AutoScope::Song) {
            let pts = &mut lane.points;
            let inside = pts.iter().any(|p| p.tick >= a && p.tick < b);
            let across = pts.iter().any(|p| p.tick < a) && pts.iter().any(|p| p.tick >= b);
            if !(inside || across) {
                continue;
            }
            pin(pts, b);
            if a > 0 && pts.iter().any(|p| p.tick < a) {
                pin(pts, a - 1);
            }
            pts.retain(|p| p.tick < a || p.tick >= b);
        }
        let changed = self.tempo_changes.iter().any(|p| p.tick >= a && p.tick < b);
        self.tempo_changes.retain(|p| p.tick < a || p.tick >= b);
        if changed && !self.tempo_changes.iter().any(|p| p.tick == b) {
            self.set_tempo_change(b, lane_map.bpm_at(b as f64) as f32, false);
        }
        let changed = self.signatures.iter().any(|s| s.tick >= a && s.tick < b);
        self.signatures.retain(|s| s.tick < a || s.tick >= b);
        if changed && !self.signatures.iter().any(|s| s.tick == b) {
            let s = lane_map.sig_at(b as f64);
            self.set_signature(b, s.num, s.den);
        }
        self.markers.retain(|m| m.tick < a || m.tick >= b);
        self.normalize_tempo();
    }

    /// Moves everything that starts at or after `at` by `delta` ticks.
    fn shift_from(&mut self, at: u32, delta: i64) {
        for c in self.clips.iter_mut().filter(|c| c.start >= at) {
            c.start = shifted(c.start, delta);
        }
        for c in self.audio_clips.iter_mut().filter(|c| c.start >= at) {
            c.start = shifted(c.start, delta);
        }
        for lane in self.automation.iter_mut().filter(|l| l.scope == AutoScope::Song) {
            for p in lane.points.iter_mut().filter(|p| p.tick >= at) {
                p.tick = shifted(p.tick, delta);
            }
        }
        for p in self.tempo_changes.iter_mut().filter(|p| p.tick >= at) {
            p.tick = shifted(p.tick, delta);
        }
        for s in self.signatures.iter_mut().filter(|s| s.tick >= at) {
            s.tick = shifted(s.tick, delta);
        }
        for m in self.markers.iter_mut().filter(|m| m.tick >= at) {
            m.tick = shifted(m.tick, delta);
        }
    }

    /// Removes `[a, b)` and closes the gap: what follows moves left.
    pub fn delete_range(&mut self, a: u32, b: u32) {
        if b <= a {
            return;
        }
        self.clear_range(a, b);
        self.shift_from(b, -((b - a) as i64));
        self.normalize_tempo();
    }

    /// Opens an empty gap of `len` ticks at `at`: clips across it are split,
    /// what follows moves right.
    pub fn insert_gap(&mut self, at: u32, len: u32) {
        if len == 0 {
            return;
        }
        let map = self.tempo_map();
        let lane_map = self.tempo_lane_map();
        let mut clips = Vec::with_capacity(self.clips.len() + 1);
        for c in &self.clips {
            if c.start < at && c.start + c.len > at {
                let left = at - c.start;
                clips.push(ClipDef { len: left, ..*c });
                clips.push(ClipDef { start: at, len: c.len - left, offset: c.offset + left, ..*c });
            } else {
                clips.push(*c);
            }
        }
        self.clips = clips;
        let mut audio = Vec::with_capacity(self.audio_clips.len() + 1);
        for c in &self.audio_clips {
            if c.start < at && c.start + c.len > at {
                let left = at - c.start;
                let offset = c.offset_at(at as f64, &map);
                audio.push(AudioClipDef { len: left, fade_out: 0.0, ..c.clone() });
                audio.push(AudioClipDef { start: at, len: c.len - left, offset, fade_in: 0.0, ..c.clone() });
            } else {
                audio.push(c.clone());
            }
        }
        self.audio_clips = audio;
        for lane in self.automation.iter_mut().filter(|l| l.scope == AutoScope::Song) {
            if lane.points.iter().any(|p| p.tick < at) && lane.points.iter().any(|p| p.tick > at) {
                pin(&mut lane.points, at);
            }
        }
        // A ramp across the gap ends at `at` and the rest restarts after it.
        let ramp_across = self.tempo_changes.iter().find(|p| p.tick > at).is_some_and(|p| p.ramp)
            && !self.tempo_changes.iter().any(|p| p.tick == at);
        let bpm_here = lane_map.bpm_at(at as f64) as f32;
        self.shift_from(at, len as i64);
        if ramp_across {
            self.set_tempo_change(at, bpm_here, true);
            self.set_tempo_change(at + len, bpm_here, false);
        }
    }

    /// Lays `slice` at `at`, over whatever was in `[at, at + len)`, which
    /// should be empty (see [`Project::clear_range`], [`Project::insert_gap`]).
    pub fn paste(&mut self, slice: &TimeSlice, at: u32) {
        let end = at + slice.len;
        let lane_map = self.tempo_lane_map();
        let (tail_bpm, tail_sig) = (lane_map.bpm_at(end as f64) as f32, lane_map.sig_at(end as f64));
        self.clips.extend(slice.clips.iter().map(|c| ClipDef { start: c.start + at, ..*c }));
        self.audio_clips.extend(slice.audio.iter().map(|c| AudioClipDef { start: c.start + at, ..c.clone() }));
        for (target, pts) in &slice.lanes {
            if self.automation_lane(*target, AutoScope::Song).is_none() {
                self.automation.push(crate::AutomationDef { target: *target, scope: AutoScope::Song, points: Vec::new() });
            }
            let lane = self.automation_lane_mut(*target, AutoScope::Song).expect("lane created");
            let tail = lane.value_at(end as f64);
            lane.points.retain(|p| p.tick < at || p.tick >= end);
            let had_tail = lane.points.iter().any(|p| p.tick == end);
            let i = lane.points.partition_point(|p| p.tick < at);
            lane.points.splice(i..i, pts.iter().map(|p| AutoPoint { tick: p.tick + at, ..*p }));
            if let Some(value) = tail.filter(|_| !had_tail) {
                let i = lane.points.partition_point(|p| p.tick < end);
                lane.points.insert(i, AutoPoint { tick: end, value, curve: 0.0 });
            }
        }
        self.tempo_changes.retain(|p| p.tick < at || p.tick >= end);
        let had_tail = self.tempo_changes.iter().any(|p| p.tick == end);
        for p in &slice.tempos {
            self.set_tempo_change(p.tick + at, p.bpm, p.ramp && p.tick > 0);
        }
        if !had_tail {
            self.set_tempo_change(end, tail_bpm, false);
        }
        self.signatures.retain(|s| s.tick < at || s.tick >= end);
        let had_tail = self.signatures.iter().any(|s| s.tick == end);
        for s in &slice.sigs {
            self.set_signature(s.tick + at, s.num, s.den);
        }
        if !had_tail {
            self.set_signature(end, tail_sig.num, tail_sig.den);
        }
        for m in &slice.markers {
            let i = self.add_marker(m.tick + at, m.name.clone());
            self.markers[i].color = m.color;
        }
        self.normalize_tempo();
    }

    /// Moves section `i` so that it starts at `to`. Insert: the section is
    /// taken out (gap closed) and slid in at `to`; overwrite: it replaces what
    /// was there and leaves its place empty. Returns the new section index.
    pub fn move_section(&mut self, i: usize, to: u32, placement: Placement) -> Option<usize> {
        let (a, b) = self.section(i)?;
        let len = b - a;
        let slice = self.extract(a, b);
        let start = match placement {
            Placement::Insert => {
                self.delete_range(a, b);
                let to = if to >= b { to - len } else if to > a { a } else { to };
                self.insert_gap(to, len);
                to
            }
            Placement::Overwrite => {
                self.clear_range(a, b);
                self.clear_range(to, to + len);
                to
            }
        };
        self.paste(&slice, start);
        self.markers.iter().position(|m| m.tick == start)
    }

    /// Copies section `i` to `to` (insert or overwrite). Returns the index of the copy.
    pub fn duplicate_section(&mut self, i: usize, to: u32, placement: Placement) -> Option<usize> {
        let (a, b) = self.section(i)?;
        let slice = self.extract(a, b);
        match placement {
            Placement::Insert => self.insert_gap(to, b - a),
            Placement::Overwrite => self.clear_range(to, to + (b - a)),
        }
        self.paste(&slice, to);
        self.markers.iter().position(|m| m.tick == to)
    }

    /// Deletes section `i` and its content; the rest of the song moves left.
    pub fn delete_section(&mut self, i: usize) -> bool {
        let Some((a, b)) = self.section(i) else { return false };
        self.delete_range(a, b);
        true
    }

    /// Default marker names, in order.
    pub fn next_marker_name(&self) -> String {
        const NAMES: [&str; 8] = [
            ponzik_i18n::tr_noop("Intro"),
            ponzik_i18n::tr_noop("Verse"),
            ponzik_i18n::tr_noop("Chorus"),
            ponzik_i18n::tr_noop("Bridge"),
            ponzik_i18n::tr_noop("Drop"),
            ponzik_i18n::tr_noop("Break"),
            ponzik_i18n::tr_noop("Build-up"),
            ponzik_i18n::tr_noop("Outro"),
        ];
        ponzik_i18n::tr(NAMES[self.markers.len() % NAMES.len()]).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ponzik_engine::{TICKS_PER_BAR as BAR, TICKS_PER_STEP};

    fn clip(pattern: usize, start: u32, len: u32) -> ClipDef {
        ClipDef { pattern, lane: 0, start, len, offset: 0, muted: false }
    }

    /// Three sections of two bars: A (pattern 0), B (pattern 1), C (pattern 2).
    fn song() -> Project {
        let mut p = Project::empty();
        p.patterns = (0..3).map(|i| crate::PatternDef { name: format!("P{i}"), len: BAR, notes: Vec::new(), color: 0 }).collect();
        for (i, start) in [0, 2 * BAR, 4 * BAR].into_iter().enumerate() {
            p.clips.push(clip(i, start, 2 * BAR));
            p.add_marker(start, ["A", "B", "C"][i]);
        }
        // Volume ramp across B.
        let gain = AutoTarget::TrackGain(1);
        p.add_automation_point(gain, AutoScope::Song, 2 * BAR, 0.2);
        p.add_automation_point(gain, AutoScope::Song, 4 * BAR - 1, 0.8);
        p
    }

    fn layout(p: &Project) -> Vec<(usize, u32)> {
        let mut v: Vec<(usize, u32)> = p.clips.iter().map(|c| (c.pattern, c.start)).collect();
        v.sort_by_key(|c| c.1);
        v
    }

    fn names(p: &Project) -> Vec<(&str, u32)> {
        p.markers.iter().map(|m| (m.name.as_str(), m.tick)).collect()
    }

    #[test]
    fn sections_run_from_marker_to_marker() {
        let p = song();
        assert_eq!(p.sections(), [(0, 2 * BAR), (2 * BAR, 4 * BAR), (4 * BAR, 6 * BAR)]);
        assert_eq!(p.section(3), None);
    }

    #[test]
    fn move_section_insert_reorders_the_song() {
        let mut p = song();
        // A to the end: B C A.
        let i = p.move_section(0, 6 * BAR, Placement::Insert).unwrap();
        assert_eq!(layout(&p), [(1, 0), (2, 2 * BAR), (0, 4 * BAR)]);
        assert_eq!(names(&p), [("B", 0), ("C", 2 * BAR), ("A", 4 * BAR)]);
        assert_eq!(i, 2);
        // The automation moved with B, to the start.
        let lane = p.automation_lane(AutoTarget::TrackGain(1), AutoScope::Song).unwrap();
        assert_eq!(lane.value_at(0.0), Some(0.2));
        assert!((lane.value_at((2 * BAR - 1) as f64).unwrap() - 0.8).abs() < 1e-6);
        // C back before B: C B A.
        p.move_section(1, 0, Placement::Insert).unwrap();
        assert_eq!(layout(&p), [(2, 0), (1, 2 * BAR), (0, 4 * BAR)]);
        assert_eq!(p.song_len(), 6 * BAR);
    }

    #[test]
    fn move_section_overwrite_leaves_a_gap() {
        let mut p = song();
        p.move_section(0, 4 * BAR, Placement::Overwrite).unwrap();
        assert_eq!(layout(&p), [(1, 2 * BAR), (0, 4 * BAR)]);
        assert_eq!(names(&p), [("B", 2 * BAR), ("A", 4 * BAR)]);
    }

    #[test]
    fn duplicate_section_insert_and_overwrite() {
        let mut p = song();
        let i = p.duplicate_section(1, 4 * BAR, Placement::Insert).unwrap();
        assert_eq!(i, 2);
        assert_eq!(layout(&p), [(0, 0), (1, 2 * BAR), (1, 4 * BAR), (2, 6 * BAR)]);
        assert_eq!(names(&p), [("A", 0), ("B", 2 * BAR), ("B", 4 * BAR), ("C", 6 * BAR)]);
        let lane = p.automation_lane(AutoTarget::TrackGain(1), AutoScope::Song).unwrap();
        assert_eq!(lane.value_at((4 * BAR) as f64), Some(0.2));
        let mut q = song();
        q.duplicate_section(0, 4 * BAR, Placement::Overwrite).unwrap();
        assert_eq!(layout(&q), [(0, 0), (1, 2 * BAR), (0, 4 * BAR)]);
        assert_eq!(names(&q), [("A", 0), ("B", 2 * BAR), ("A", 4 * BAR)]);
    }

    #[test]
    fn delete_section_closes_the_gap() {
        let mut p = song();
        assert!(p.delete_section(1));
        assert_eq!(layout(&p), [(0, 0), (2, 2 * BAR)]);
        assert_eq!(names(&p), [("A", 0), ("C", 2 * BAR)]);
        assert_eq!(p.song_len(), 4 * BAR);
        // The automation of B went with it; A and C keep their values.
        let lane = p.automation_lane(AutoTarget::TrackGain(1), AutoScope::Song).unwrap();
        assert!(lane.points.iter().all(|pt| pt.value > 0.79 || pt.value < 0.21));
    }

    #[test]
    fn clips_across_section_edges_are_cut() {
        let mut p = song();
        p.clips.push(clip(0, BAR, 4 * BAR + TICKS_PER_STEP));
        p.delete_section(1);
        let mut pieces: Vec<(u32, u32, u32)> = p.clips.iter().filter(|c| c.pattern == 0).map(|c| (c.start, c.len, c.offset)).collect();
        pieces.sort();
        assert_eq!(pieces, [(0, 2 * BAR, 0), (BAR, BAR, 0), (2 * BAR, BAR + TICKS_PER_STEP, 3 * BAR)]);
    }

    #[test]
    fn tempo_and_signature_travel_with_their_section() {
        let mut p = song();
        p.set_signature(2 * BAR, 3, 4);
        p.set_signature(2 * BAR + 2 * 288, 4, 4);
        // B is now two bars of 3/4: 576 ticks; C starts after it.
        p.markers[2].tick = 2 * BAR + 576;
        p.clips[2].start = 2 * BAR + 576;
        p.set_tempo_change(2 * BAR, 90.0, false);
        p.set_tempo_change(2 * BAR + 576, 120.0, false);
        p.move_section(1, 0, Placement::Insert).unwrap();
        let map = p.tempo_map();
        assert_eq!(map.sig_at(0.0), TimeSig { tick: 0, num: 3, den: 4 });
        assert_eq!(map.sig_at(600.0).num, 4);
        assert_eq!(map.bpm_at(10.0), 90.0);
        assert_eq!(map.bpm_at(600.0), 120.0);
        assert_eq!(p.bpm, 90.0);
        assert_eq!(names(&p), [("B", 0), ("A", 576), ("C", 576 + 2 * BAR)]);
        assert_eq!(map.bar_beat(576.0).bar, 2);
    }

    #[test]
    fn normalize_drops_redundant_changes() {
        let mut p = Project::empty();
        p.set_tempo_change(384, 120.0, false);
        p.set_tempo_change(768, 140.0, true);
        p.set_signature(0, 4, 4);
        p.set_signature(768, 4, 4);
        p.set_signature(1152, 3, 4);
        p.normalize_tempo();
        // The point before the ramp stays: the ramp starts from it.
        assert_eq!(p.tempo_changes.len(), 2);
        assert_eq!(p.signatures, [TimeSig { tick: 1152, num: 3, den: 4 }]);
    }

    #[test]
    fn tempo_point_editing() {
        let mut p = Project::empty();
        assert_eq!(p.set_tempo_change(384, 140.0, false), 1);
        assert_eq!(p.tempo_point_at(100.0), 0);
        assert_eq!(p.tempo_point_at(384.0), 1);
        p.set_tempo_point_bpm(1, 150.0);
        p.set_tempo_point_bpm(0, 100.0);
        assert_eq!((p.tempo_point_bpm(0), p.tempo_point_bpm(1)), (100.0, 150.0));
        assert_eq!(p.set_tempo_change(0, 110.0, true), 0);
        assert_eq!(p.bpm, 110.0);
    }
}
