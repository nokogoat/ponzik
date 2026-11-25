//! Performance mode, after FL Studio: the playlist's clips from the
//! performance marker on are launched live, one at a time per lane.
//!
//! The player plans every start and stop ahead of time on the launch
//! quantization, like the Session view's ([`crate::session::Player`]): the
//! engine gets each clip with its start and end in ticks before they happen,
//! so launches land on the exact sample. A launched clip loops its own span
//! (its length and offset in the playlist) until something replaces it on
//! its lane. Performance mode plays in the engine's session mode, at the
//! song's first tempo.
//!
//! Recording a performance writes the clips as they played into the
//! arrangement (left of the marker), one playlist clip per loop; the zone
//! moves right when the recording runs into it.

use ponzik_engine::{AudioClip, ClipWarp, SessionSlot, TICKS_PER_STEP};

use crate::live::{PerfLaunch, PerfLive};
use crate::{AudioClipDef, ClipDef, Project, SampleLoader, WarpLookup};

/// A playlist clip of the performance zone, as launched.
#[derive(Clone, Debug, PartialEq)]
pub enum PerfClip {
    Pattern(ClipDef),
    Audio(AudioClipDef),
}

impl PerfClip {
    pub fn lane(&self) -> usize {
        match self {
            Self::Pattern(c) => c.lane,
            Self::Audio(c) => c.lane,
        }
    }

    pub fn start(&self) -> u32 {
        match self {
            Self::Pattern(c) => c.start,
            Self::Audio(c) => c.start,
        }
    }

    /// Length of one loop, in ticks.
    pub fn len(&self) -> u32 {
        match self {
            Self::Pattern(c) => c.len.max(1),
            Self::Audio(c) => c.len.max(1),
        }
    }
}

/// Which playlist clip: a pattern clip or an audio clip, by index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PerfKey {
    Pattern(usize),
    Audio(usize),
}

impl Project {
    /// The zone's clip behind `key` (not muted), if there is one.
    pub fn perf_clip(&self, key: PerfKey) -> Option<PerfClip> {
        let perf = &self.live.perf;
        match key {
            PerfKey::Pattern(i) => {
                self.clips.get(i).filter(|c| !c.muted && perf.in_zone(c.start)).map(|c| PerfClip::Pattern(*c))
            }
            PerfKey::Audio(i) => {
                self.audio_clips.get(i).filter(|c| !c.muted && perf.in_zone(c.start)).map(|c| PerfClip::Audio(c.clone()))
            }
        }
    }

    /// The zone's clips lane by lane (lanes in order), each lane's by start.
    pub fn perf_lanes(&self) -> Vec<(usize, Vec<PerfKey>)> {
        let mut all: Vec<(usize, u32, PerfKey)> = Vec::new();
        for (i, c) in self.clips.iter().enumerate() {
            if self.perf_clip(PerfKey::Pattern(i)).is_some() {
                all.push((c.lane, c.start, PerfKey::Pattern(i)));
            }
        }
        for (i, c) in self.audio_clips.iter().enumerate() {
            if self.perf_clip(PerfKey::Audio(i)).is_some() {
                all.push((c.lane, c.start, PerfKey::Audio(i)));
            }
        }
        all.sort_by_key(|&(lane, start, _)| (lane, start));
        let mut out: Vec<(usize, Vec<PerfKey>)> = Vec::new();
        for (lane, _, key) in all {
            match out.last_mut() {
                Some((l, keys)) if *l == lane => keys.push(key),
                _ => out.push((lane, vec![key])),
            }
        }
        out
    }

    /// Clips of the zone as a launch grid: column `c` is the `c`-th lane
    /// that holds some, row `r` its `r`-th clip.
    pub fn perf_grid(&self, cols: usize, rows: usize) -> Vec<Vec<PerfKey>> {
        self.perf_lanes().into_iter().take(cols).map(|(_, keys)| keys.into_iter().take(rows).collect()).collect()
    }

    /// Audio clips of the zone (warped ones are rendered at the session tempo).
    pub(crate) fn perf_audio_defs(&self) -> Vec<&AudioClipDef> {
        self.audio_clips.iter().filter(|c| !c.muted && self.live.perf.in_zone(c.start)).collect()
    }

    /// Engine slots of the performance's pattern clips: each loops its span.
    pub(crate) fn perf_slots(&self) -> Vec<SessionSlot> {
        self.live
            .playing
            .iter()
            .filter_map(|l| match &l.clip {
                PerfClip::Pattern(c) if c.pattern < self.patterns.len() => Some(SessionSlot {
                    pattern: c.pattern,
                    start: l.start,
                    end: l.end,
                    offset: c.offset,
                    period: c.len.max(1),
                    soft: 0,
                }),
                _ => None,
            })
            .collect()
    }

    /// Engine clips of the performance's audio clips: each loops its span.
    pub(crate) fn perf_audio(&self, sample_rate: f32, loader: SampleLoader, warps: WarpLookup) -> Vec<AudioClip> {
        let tempo = self.session_tempo();
        self.live
            .playing
            .iter()
            .filter_map(|l| {
                let PerfClip::Audio(def) = &l.clip else { return None };
                let sample = loader(&def.path)?;
                let cycle = def.len.max(1);
                let job_def = AudioClipDef { start: 0, len: cycle, ..def.clone() };
                let warp = def.warp.then(|| ClipWarp {
                    map: def.warp_map(),
                    rendered: crate::warp::WarpJob::for_clip(&job_def, &sample, &tempo, sample_rate).and_then(|job| warps(&job)),
                });
                Some(AudioClip {
                    sample,
                    track: def.track.min(ponzik_engine::mixer::MAX_TRACKS - 1),
                    start: l.start,
                    len: l.end.saturating_sub(l.start).max(1),
                    offset: def.offset,
                    gain: def.gain,
                    fade_in: def.fade_in,
                    fade_out: def.fade_out,
                    fade_in_curve: def.fade_in_curve,
                    fade_out_curve: def.fade_out_curve,
                    reverse: def.reverse,
                    looped: def.looped,
                    warp,
                    cycle,
                    phase: 0,
                    xfade_in: 0.0,
                    xfade_out: 0.0,
                })
            })
            .collect()
    }

    /// Writes a recorded performance into the arrangement, at the ticks it
    /// played: one playlist clip per loop of each launched clip. When the
    /// recording runs into the zone, the zone (marker and clips) first moves
    /// right to the next bar after it. Returns how many clips were written.
    pub fn write_performance(&mut self, takes: &[PerfTake]) -> usize {
        let takes: Vec<&PerfTake> = takes.iter().filter(|t| t.end > t.from).collect();
        let Some(end) = takes.iter().map(|t| t.end).max() else { return 0 };
        let zone = self.live.perf.zone;
        if self.live.perf.on && end > zone {
            let map = self.tempo_map();
            let to = (map.ceil_bar(end as f64) as u32).max(end);
            let delta = to - zone;
            for c in self.clips.iter_mut().filter(|c| c.start >= zone) {
                c.start += delta;
            }
            for c in self.audio_clips.iter_mut().filter(|c| c.start >= zone) {
                c.start += delta;
            }
            self.live.perf.zone = to;
        }
        let tempo = self.session_tempo();
        let before = self.clips.len() + self.audio_clips.len();
        for t in takes {
            let len = t.clip.len();
            // Loops from the first one the take covers.
            let mut cycle = t.start + (t.from.saturating_sub(t.start) / len) * len;
            while cycle < t.end {
                let (s, e) = (t.from.max(cycle), t.end.min(cycle.saturating_add(len)));
                if s < e {
                    let phase = s - cycle;
                    match &t.clip {
                        PerfClip::Pattern(c) => {
                            self.clips.push(ClipDef { lane: t.lane, start: s, len: e - s, offset: c.offset + phase, muted: false, ..*c });
                        }
                        PerfClip::Audio(c) => {
                            let base = AudioClipDef { start: 0, ..c.clone() };
                            let offset = if phase > 0 { base.offset_at(phase as f64, &tempo) } else { c.offset };
                            self.audio_clips.push(AudioClipDef { lane: t.lane, start: s, len: e - s, offset, ..c.clone() });
                        }
                    }
                }
                cycle = cycle.saturating_add(len);
            }
        }
        self.clips.len() + self.audio_clips.len() - before
    }
}

// --- Player ---

/// A clip playing on a lane, from `start`.
#[derive(Clone, Debug, PartialEq)]
pub struct Play {
    pub key: PerfKey,
    pub clip: PerfClip,
    pub start: u32,
}

/// A start (or a stop, `None`) planned at `at`.
#[derive(Clone, Debug, PartialEq)]
pub struct Planned {
    pub at: u32,
    pub play: Option<Play>,
}

/// One lane of the player: what plays, what is planned, what is held.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Lane {
    pub playing: Option<Play>,
    /// Sorted by tick.
    pub pending: Vec<Planned>,
    /// Clip whose pad is held (gate).
    pub held: Option<PerfKey>,
}

impl Lane {
    /// What the lane ends up playing, after every planned step.
    fn last(&self) -> Option<&Play> {
        match self.pending.last() {
            Some(p) => p.play.as_ref(),
            None => self.playing.as_ref(),
        }
    }

    /// A step planned at `at`: the steps planned after it go.
    fn plan(&mut self, at: u32, play: Option<Play>) {
        self.pending.retain(|x| x.at < at);
        self.pending.push(Planned { at, play });
    }

    /// The launch waiting to start, as shown (clip, tick).
    pub fn queued(&self) -> Option<(PerfKey, u32)> {
        self.pending.iter().find_map(|p| p.play.as_ref().map(|pl| (pl.key, p.at)))
    }

    /// A stop is waiting.
    pub fn stopping(&self) -> bool {
        self.pending.iter().any(|p| p.play.is_none())
    }

    /// True if nothing plays or waits.
    pub fn is_idle(&self) -> bool {
        self.playing.is_none() && self.pending.is_empty()
    }
}

/// A clip as it played, for the arrangement: its lane, the clip, the start
/// of its loops (`start`), and the span recorded (`from` to `end`).
#[derive(Clone, Debug, PartialEq)]
pub struct PerfTake {
    pub lane: usize,
    pub clip: PerfClip,
    pub start: u32,
    pub from: u32,
    pub end: u32,
}

/// Performance recording from `from` on.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PerfRecorder {
    pub from: u32,
    pub takes: Vec<PerfTake>,
}

/// The performance's playing state (not saved).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PerfPlayer {
    /// By playlist lane, grown as clips are launched.
    pub lanes: Vec<Lane>,
    pub recorder: Option<PerfRecorder>,
}

impl PerfPlayer {
    pub fn lane(&self, lane: usize) -> Option<&Lane> {
        self.lanes.get(lane)
    }

    fn lane_mut(&mut self, lane: usize) -> &mut Lane {
        if self.lanes.len() <= lane {
            self.lanes.resize_with(lane + 1, Lane::default);
        }
        &mut self.lanes[lane]
    }

    /// True if nothing plays or waits.
    pub fn is_idle(&self) -> bool {
        self.lanes.iter().all(Lane::is_idle)
    }

    /// The clip's pad goes down at `pos`; true if anything was planned.
    pub fn press(&mut self, p: &Project, key: PerfKey, pos: f64, playing: bool) -> bool {
        let Some(clip) = p.perf_clip(key) else { return false };
        let perf = p.live.perf;
        let at = p.launch_at(pos, perf.quant, playing);
        let l = self.lane_mut(clip.lane());
        let same = l.last().is_some_and(|x| x.key == key);
        match perf.launch {
            PerfLaunch::Trigger if same => return false,
            PerfLaunch::Toggle if same => {
                l.plan(at, None);
                return true;
            }
            PerfLaunch::Gate => l.held = Some(key),
            _ => {}
        }
        l.plan(at, Some(Play { key, clip, start: at }));
        true
    }

    /// The clip's pad comes up at `pos`: a gate clip stops on the next
    /// boundary, after one step at least.
    pub fn release(&mut self, p: &Project, key: PerfKey, pos: f64, playing: bool) {
        let perf = p.live.perf;
        let Some(clip) = p.perf_clip(key) else { return };
        let lane = clip.lane();
        if perf.launch != PerfLaunch::Gate || self.lane(lane).is_none_or(|l| l.held != Some(key)) {
            return;
        }
        let step = p.quant_ticks(perf.quant).max(TICKS_PER_STEP);
        let l = self.lane_mut(lane);
        l.held = None;
        if !playing {
            *l = Lane::default();
            return;
        }
        let started = l.pending.iter().rev().find_map(|x| x.play.as_ref().filter(|pl| pl.key == key).map(|_| x.at));
        let started = started.or_else(|| l.playing.as_ref().filter(|pl| pl.key == key).map(|pl| pl.start));
        let Some(started) = started else { return };
        let at = p.launch_at(pos, perf.quant, playing).max(started + step);
        l.plan(at, None);
    }

    /// Stops a lane on the launch quantization (right away when stopped).
    pub fn stop_lane(&mut self, p: &Project, lane: usize, pos: f64, playing: bool) {
        if self.lane(lane).is_none_or(Lane::is_idle) {
            return;
        }
        let at = p.launch_at(pos, p.live.perf.quant, playing);
        let l = self.lane_mut(lane);
        l.held = None;
        if playing {
            l.plan(at, None);
        } else {
            *l = Lane::default();
        }
    }

    /// Stops every lane.
    pub fn stop_all(&mut self, p: &Project, pos: f64, playing: bool) {
        for lane in 0..self.lanes.len() {
            self.stop_lane(p, lane, pos, playing);
        }
    }

    /// Everything stops now (transport stopped, mode left); a recording goes on.
    pub fn reset(&mut self) {
        let recorder = self.recorder.take();
        *self = Self { recorder, ..Self::default() };
    }

    /// Steps whose tick has come happen. True if anything changed.
    pub fn advance(&mut self, pos: f64) -> bool {
        let now = pos.max(0.0);
        let mut changed = false;
        for lane in 0..self.lanes.len() {
            while self.lanes[lane].pending.first().is_some_and(|x| x.at as f64 <= now) {
                let x = self.lanes[lane].pending.remove(0);
                changed = true;
                self.close_take(lane, x.at);
                let l = &mut self.lanes[lane];
                if x.play.is_none() {
                    l.held = None;
                }
                l.playing = x.play;
                self.open_take(lane, x.at);
            }
        }
        changed
    }

    /// The clips for the engine: each playing or planned clip, until the
    /// next step of its lane.
    pub fn live(&self) -> Vec<PerfLive> {
        let mut out = Vec::new();
        for (lane, l) in self.lanes.iter().enumerate() {
            let mut cur = l.playing.as_ref();
            for x in &l.pending {
                if let Some(pl) = cur {
                    out.push(PerfLive { lane, clip: pl.clip.clone(), start: pl.start, end: x.at.max(pl.start) });
                }
                cur = x.play.as_ref();
            }
            if let Some(pl) = cur {
                out.push(PerfLive { lane, clip: pl.clip.clone(), start: pl.start, end: u32::MAX });
            }
        }
        out.retain(|l| l.end > l.start);
        out
    }

    /// Where the clip of `key` is in its loop at `pos` (0..1), if it plays.
    pub fn progress(&self, key: PerfKey, pos: f64) -> Option<f32> {
        self.lanes.iter().find_map(|l| {
            let pl = l.playing.as_ref().filter(|pl| pl.key == key && pos >= pl.start as f64)?;
            let len = pl.clip.len() as f64;
            Some(((pos - pl.start as f64).rem_euclid(len) / len) as f32)
        })
    }

    /// True if the clip of `key` plays now.
    pub fn is_playing(&self, key: PerfKey) -> bool {
        self.lanes.iter().any(|l| l.playing.as_ref().is_some_and(|pl| pl.key == key))
    }

    /// True if the clip of `key` waits to start.
    pub fn is_queued(&self, key: PerfKey) -> bool {
        self.lanes.iter().any(|l| l.queued().is_some_and(|(k, _)| k == key))
    }

    // --- Recording ---

    /// Starts recording the performance at `pos`.
    pub fn start_recording(&mut self, pos: f64) {
        let from = pos.max(0.0) as u32;
        self.recorder = Some(PerfRecorder { from, takes: Vec::new() });
        for lane in 0..self.lanes.len() {
            self.open_take(lane, from);
        }
    }

    /// Stops recording at `pos`: the takes, clips still playing cut there.
    pub fn finish_recording(&mut self, pos: f64) -> Vec<PerfTake> {
        let end = pos.max(0.0) as u32;
        for lane in 0..self.lanes.len() {
            self.close_take(lane, end);
        }
        self.recorder.take().map(|r| r.takes).unwrap_or_default()
    }

    fn open_take(&mut self, lane: usize, at: u32) {
        let Some(pl) = self.lanes.get(lane).and_then(|l| l.playing.clone()) else { return };
        let Some(rec) = &mut self.recorder else { return };
        let from = at.max(rec.from).max(pl.start);
        rec.takes.push(PerfTake { lane, clip: pl.clip, start: pl.start, from, end: u32::MAX });
    }

    fn close_take(&mut self, lane: usize, at: u32) {
        let Some(rec) = &mut self.recorder else { return };
        if let Some(t) = rec.takes.iter_mut().rev().find(|t| t.lane == lane && t.end == u32::MAX) {
            t.end = at.max(t.from);
        }
        rec.takes.retain(|t| t.end > t.from);
    }
}

/// Number of the performance clip under a key of the typing keyboard's
/// number row, as a column of the launch grid (the same clip number on
/// every lane): keys 1 to 8.
pub fn column_key(code: u16) -> Option<usize> {
    (2..10).contains(&code).then(|| (code - 2) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live::Perf;
    use crate::session::Quant;
    use ponzik_engine::sequence::SeqEvent;
    use ponzik_engine::{PPQ, PlayMode, TICKS_PER_BAR};

    const BAR: u32 = TICKS_PER_BAR;

    /// The demo's patterns, no arrangement; the zone at bar 9 holds three
    /// clips on lane 0 (one bar, two bars, half a bar with an offset) and
    /// one on lane 1.
    fn project() -> Project {
        let mut p = crate::demo_project();
        p.clips.clear();
        p.audio_clips.clear();
        p.take_groups.clear();
        p.automation.clear();
        p.tempo_changes.clear();
        p.live.perf = Perf { on: true, zone: 8 * BAR, quant: Quant::Bar, launch: PerfLaunch::Trigger };
        let z = 8 * BAR;
        p.clips.push(ClipDef { pattern: 0, lane: 0, start: z, len: BAR, offset: 0, muted: false });
        p.clips.push(ClipDef { pattern: 1, lane: 0, start: z + BAR, len: 2 * BAR, offset: 0, muted: false });
        p.clips.push(ClipDef { pattern: 2, lane: 0, start: z + 3 * BAR, len: BAR / 2, offset: PPQ, muted: false });
        p.clips.push(ClipDef { pattern: 1, lane: 1, start: z, len: BAR, offset: 0, muted: false });
        // An arrangement clip, left of the marker: not launchable.
        p.clips.push(ClipDef { pattern: 0, lane: 2, start: 0, len: BAR, offset: 0, muted: false });
        p
    }

    fn key(i: usize) -> PerfKey {
        PerfKey::Pattern(i)
    }

    #[test]
    fn the_zone_is_a_grid_of_lanes() {
        let p = project();
        assert_eq!(p.perf_lanes(), [(0, vec![key(0), key(1), key(2)]), (1, vec![key(3)])]);
        assert_eq!(p.perf_clip(key(4)), None, "left of the marker: the arrangement");
        assert_eq!(p.perf_grid(1, 2), [vec![key(0), key(1)]]);
        let mut off = p.clone();
        off.live.perf.on = false;
        assert!(off.perf_lanes().is_empty());
        // Song playback leaves the zone out while the mode is on.
        let seq = p.sequence_with(&|_| None);
        assert_eq!(seq.clips.len(), 1);
        assert_eq!(off.sequence_with(&|_| None).clips.len(), 5);
    }

    #[test]
    fn launches_land_on_their_quantization() {
        let mut p = project();
        for (quant, at) in [(Quant::None, 103), (Quant::Beat, 192), (Quant::Bar, BAR), (Quant::Bars2, 2 * BAR)] {
            p.live.perf.quant = quant;
            let mut pl = PerfPlayer::default();
            assert!(pl.press(&p, key(0), 100.3, true));
            assert_eq!(pl.lane(0).unwrap().queued(), Some((key(0), at)), "{quant:?}");
            assert!(!pl.advance(at as f64 - 1.0));
            assert!(pl.advance(at as f64));
            assert!(pl.is_playing(key(0)));
            assert_eq!(pl.live()[0].start, at);
        }
        // Stopped transport: from the start.
        let mut pl = PerfPlayer::default();
        pl.press(&p, key(1), 5000.0, false);
        assert_eq!(pl.lane(0).unwrap().queued(), Some((key(1), 0)));
        // An arrangement clip does not launch.
        assert!(!pl.press(&p, key(4), 0.0, true));
    }

    #[test]
    fn one_clip_per_lane_and_lanes_are_independent() {
        let p = project();
        let mut pl = PerfPlayer::default();
        pl.press(&p, key(0), 0.0, false);
        pl.press(&p, key(3), 0.0, false);
        pl.advance(0.0);
        // Clip 1 on the same lane replaces clip 0 on the next bar; lane 1 plays on.
        pl.press(&p, key(1), 500.0, true);
        let live = pl.live();
        assert!(live.contains(&PerfLive { lane: 0, clip: p.perf_clip(key(0)).unwrap(), start: 0, end: 2 * BAR }));
        assert!(live.contains(&PerfLive { lane: 0, clip: p.perf_clip(key(1)).unwrap(), start: 2 * BAR, end: u32::MAX }));
        assert!(live.contains(&PerfLive { lane: 1, clip: p.perf_clip(key(3)).unwrap(), start: 0, end: u32::MAX }));
        assert!(pl.is_queued(key(1)) && pl.is_playing(key(0)));
        pl.advance((2 * BAR) as f64);
        assert!(pl.is_playing(key(1)) && !pl.is_playing(key(0)) && pl.is_playing(key(3)));
        // Its progress: 0.25 of its two-bar loop half a bar in.
        assert!((pl.progress(key(1), (2 * BAR + BAR / 2) as f64).unwrap() - 0.25).abs() < 1e-6);
        // Stop one lane, then everything.
        pl.stop_lane(&p, 1, (2 * BAR + 10) as f64, true);
        assert!(pl.lane(1).unwrap().stopping());
        pl.advance((3 * BAR) as f64);
        assert!(!pl.is_playing(key(3)) && pl.is_playing(key(1)));
        pl.stop_all(&p, (3 * BAR + 10) as f64, true);
        pl.advance((4 * BAR) as f64);
        assert!(pl.is_idle());
    }

    #[test]
    fn launch_modes() {
        let mut p = project();
        // Trigger: a second press on the playing clip changes nothing.
        let mut pl = PerfPlayer::default();
        pl.press(&p, key(0), 0.0, false);
        pl.advance(0.0);
        assert!(!pl.press(&p, key(0), 100.0, true));
        assert_eq!(pl.live(), [PerfLive { lane: 0, clip: p.perf_clip(key(0)).unwrap(), start: 0, end: u32::MAX }]);
        // Retrigger: it starts over on the next bar.
        p.live.perf.launch = PerfLaunch::Retrigger;
        assert!(pl.press(&p, key(0), 100.0, true));
        assert_eq!(pl.live()[1].start, BAR);
        // Toggle: the second press stops it.
        p.live.perf.launch = PerfLaunch::Toggle;
        let mut pl = PerfPlayer::default();
        pl.press(&p, key(0), 0.0, false);
        pl.advance(0.0);
        pl.press(&p, key(0), 100.0, true);
        assert!(pl.lane(0).unwrap().stopping());
        pl.advance(BAR as f64);
        assert!(pl.is_idle());
        // Gate: plays while held, a quick tap still plays one step.
        p.live.perf.launch = PerfLaunch::Gate;
        let mut pl = PerfPlayer::default();
        pl.press(&p, key(0), 100.0, true);
        pl.release(&p, key(0), 120.0, true);
        assert_eq!(pl.live(), [PerfLive { lane: 0, clip: p.perf_clip(key(0)).unwrap(), start: BAR, end: 2 * BAR }]);
        let mut pl = PerfPlayer::default();
        pl.press(&p, key(0), 100.0, true);
        pl.advance(BAR as f64);
        pl.release(&p, key(0), (2 * BAR + 50) as f64, true);
        assert_eq!(pl.live()[0].end, 3 * BAR);
        pl.advance((3 * BAR) as f64);
        assert!(pl.is_idle());
    }

    /// Notes of a sequence from `from` to `to`, sorted.
    fn notes(seq: &ponzik_engine::Sequence, from: f64, to: f64) -> Vec<SeqEvent> {
        let mut out = Vec::with_capacity(4096);
        seq.collect(from, to, &mut out);
        out
    }

    fn key_of(e: &SeqEvent) -> (i64, usize, u8, i32) {
        ((e.tick * 1000.0).round() as i64, e.channel, e.key, (e.vel * 1000.0).round() as i32)
    }

    #[test]
    fn a_recorded_performance_plays_back_the_same_from_the_arrangement() {
        let mut p = project();
        p.live.perf.quant = Quant::Beat;
        let mut pl = PerfPlayer::default();
        pl.start_recording(0.0);
        // A scripted performance: (tick, press).
        let script: [(u32, PerfKey); 6] =
            [(0, key(0)), (0, key(3)), (500, key(1)), (1300, key(2)), (2100, key(0)), (2500, key(1))];
        let end = 3000u32;
        let mut heard = Vec::new();
        let mut next = 0;
        let block = 50u32;
        let mut t = 0u32;
        p.mode = PlayMode::Session;
        while t < end {
            while next < script.len() && script[next].0 <= t {
                pl.press(&p, script[next].1, t as f64, t > 0);
                next += 1;
            }
            pl.advance(t as f64);
            // What the engine would get this block.
            p.live.playing = pl.live();
            heard.extend(notes(&p.sequence_with(&|_| None), t as f64, (t + block) as f64));
            t += block;
        }
        pl.stop_lane(&p, 1, 2000.0, true);
        let takes = pl.finish_recording(end as f64);
        assert!(pl.recorder.is_none());
        assert!(takes.len() >= 5, "{takes:?}");
        let zone_before = p.live.perf.zone;
        p.live.playing.clear();
        // The arrangement clip of the fixture would play too: out of the comparison.
        p.clips.remove(4);
        let written = p.write_performance(&takes);
        assert!(written >= takes.len());
        // The recording stays left of the marker at bar 9 (3072).
        assert_eq!(p.live.perf.zone, zone_before);
        p.mode = PlayMode::Song;
        let song = notes(&p.sequence_with(&|_| None), 0.0, end as f64);
        let mut a: Vec<_> = heard.iter().map(key_of).collect();
        let mut b: Vec<_> = song.iter().map(key_of).collect();
        a.sort_unstable();
        b.sort_unstable();
        assert!(!a.is_empty());
        assert_eq!(a, b, "the arrangement plays what was performed");
    }

    #[test]
    fn a_long_recording_pushes_the_zone_right() {
        let mut p = project();
        let mut pl = PerfPlayer::default();
        pl.press(&p, key(1), 0.0, false);
        pl.advance(0.0);
        pl.start_recording(0.0);
        // Ten bars and a beat: past the marker at bar 9.
        let end = 10 * BAR + PPQ;
        let takes = pl.finish_recording(end as f64);
        assert_eq!(takes, [PerfTake { lane: 0, clip: p.perf_clip(key(1)).unwrap(), start: 0, from: 0, end }]);
        let n = p.clips.len();
        assert_eq!(p.write_performance(&takes), 6, "five two-bar loops and a beat");
        assert_eq!(p.live.perf.zone, 11 * BAR, "the next bar after the recording");
        // The zone's clips moved with it, the arrangement did not.
        assert_eq!(p.clips[0].start, 11 * BAR);
        assert_eq!(p.clips[4].start, 0);
        let written: Vec<(u32, u32, u32)> = p.clips[n..].iter().map(|c| (c.start, c.len, c.offset)).collect();
        assert_eq!(written, [(0, 768, 0), (768, 768, 0), (1536, 768, 0), (2304, 768, 0), (3072, 768, 0), (3840, 96, 0)]);
        assert_eq!(p.perf_lanes().len(), 2, "the zone still holds its clips");

        // Recording from the middle of a loop: the first clip starts there.
        let mut p = project();
        let mut pl = PerfPlayer::default();
        pl.press(&p, key(2), 0.0, false);
        pl.advance(0.0);
        pl.start_recording(250.0);
        let takes = pl.finish_recording(400.0);
        p.write_performance(&takes);
        let got: Vec<(u32, u32, u32)> = p.clips[5..].iter().map(|c| (c.start, c.len, c.offset)).collect();
        // Half a bar (192) from offset 96: 250 is 58 into its second loop.
        assert_eq!(got, [(250, 134, PPQ + 58), (384, 16, PPQ)]);
    }
}
