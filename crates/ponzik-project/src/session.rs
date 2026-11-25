//! Session view: how slots and scenes launch (launch modes, quantization,
//! legato, follow actions), and the player that turns launches, releases and
//! follow actions into the clips the engine plays.
//!
//! The player decides everything ahead of time, on quantization boundaries:
//! the engine gets each clip's start and end in ticks before they happen, so
//! launches, follow actions and gate releases land on the exact sample. A
//! follow action is drawn as soon as its clip starts, at least a bar early.

use std::sync::Arc;

use ponzik_engine::sequence::splitmix;
use ponzik_engine::{AudioClip, ClipWarp, Sample, SessionSlot, TICKS_PER_BAR, TICKS_PER_STEP, TempoMap};
use ponzik_i18n::tr_noop;

use crate::json::Json;
use crate::{AudioClipDef, ClipDef, Project, SESSION_COLUMNS, SESSION_SCENES, SampleLoader, WarpLookup};

/// How a slot reacts to its button (mouse, key or MIDI note).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LaunchMode {
    /// A press starts the clip.
    #[default]
    Trigger,
    /// The clip plays while the button is held.
    Gate,
    /// A press starts the clip, the next one stops it.
    Toggle,
    /// While held, the clip starts over at the quantization rate.
    Repeat,
}

impl LaunchMode {
    pub const ALL: [LaunchMode; 4] = [Self::Trigger, Self::Gate, Self::Toggle, Self::Repeat];

    pub fn id(self) -> &'static str {
        match self {
            Self::Trigger => "trigger",
            Self::Gate => "gate",
            Self::Toggle => "toggle",
            Self::Repeat => "repeat",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.id() == id)
    }

    /// English label, to translate.
    pub fn label(self) -> &'static str {
        match self {
            Self::Trigger => tr_noop("Trigger"),
            Self::Gate => tr_noop("Gate"),
            Self::Toggle => tr_noop("Toggle"),
            Self::Repeat => tr_noop("Repeat"),
        }
    }

    /// The button's release matters.
    pub fn held(self) -> bool {
        matches!(self, Self::Gate | Self::Repeat)
    }
}

/// Where launches land.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Quant {
    /// Right away.
    None,
    Sixteenth,
    Beat,
    #[default]
    Bar,
    Bars2,
    Bars4,
}

impl Quant {
    pub const ALL: [Quant; 6] = [Self::None, Self::Sixteenth, Self::Beat, Self::Bar, Self::Bars2, Self::Bars4];

    /// Ticks of a step in 4/4.
    pub fn ticks(self) -> u32 {
        self.ticks_in(TICKS_PER_BAR)
    }

    /// Ticks of a step with bars of `bar` ticks (the beat stays a quarter note).
    pub fn ticks_in(self, bar: u32) -> u32 {
        match self {
            Self::None => 0,
            Self::Sixteenth => TICKS_PER_STEP,
            Self::Beat => TICKS_PER_BAR / 4,
            Self::Bar => bar.max(1),
            Self::Bars2 => 2 * bar.max(1),
            Self::Bars4 => 4 * bar.max(1),
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Sixteenth => "1/16",
            Self::Beat => "1/4",
            Self::Bar => "1",
            Self::Bars2 => "2",
            Self::Bars4 => "4",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|q| q.id() == id)
    }

    /// English label, to translate.
    pub fn label(self) -> &'static str {
        match self {
            Self::None => tr_noop("None"),
            Self::Sixteenth => "1/16",
            Self::Beat => "1/4",
            Self::Bar => tr_noop("1 bar"),
            Self::Bars2 => tr_noop("2 bars"),
            Self::Bars4 => tr_noop("4 bars"),
        }
    }
}

/// Launches closer than this to a boundary (in ticks) wait for the next one:
/// the engine may already be rendering past it.
pub const LAUNCH_AHEAD: f64 = 2.0;

/// Tick where a launch made at `pos` starts: the next boundary of `quant`
/// (right away for [`Quant::None`]), or the transport start when stopped.
pub fn launch_tick(pos: f64, quant: Quant, playing: bool) -> u32 {
    launch_tick_in(pos, quant.ticks(), playing)
}

/// Like [`launch_tick`], with a quantization step of `step` ticks (0: none).
pub fn launch_tick_in(pos: f64, step: u32, playing: bool) -> u32 {
    if !playing {
        return 0;
    }
    let at = pos.max(0.0) + LAUNCH_AHEAD;
    let q = step as f64;
    let tick = if q == 0.0 { at.ceil() } else { (at / q).ceil() * q };
    tick.min(u32::MAX as f64 / 2.0) as u32
}

/// What happens when a follow action fires.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FollowAction {
    /// The clip keeps playing.
    #[default]
    None,
    Stop,
    /// The same clip, from its start.
    Again,
    Previous,
    Next,
    First,
    Last,
    /// Any clip of the column, this one included.
    Any,
    /// Any other clip of the column.
    Other,
}

impl FollowAction {
    pub const ALL: [FollowAction; 9] =
        [Self::None, Self::Stop, Self::Again, Self::Previous, Self::Next, Self::First, Self::Last, Self::Any, Self::Other];

    pub fn id(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Stop => "stop",
            Self::Again => "again",
            Self::Previous => "previous",
            Self::Next => "next",
            Self::First => "first",
            Self::Last => "last",
            Self::Any => "any",
            Self::Other => "other",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.id() == id)
    }

    /// English label, to translate.
    pub fn label(self) -> &'static str {
        match self {
            Self::None => tr_noop("No action"),
            Self::Stop => tr_noop("Stop"),
            Self::Again => tr_noop("Play again"),
            Self::Previous => tr_noop("Previous"),
            Self::Next => tr_noop("Next"),
            Self::First => tr_noop("First"),
            Self::Last => tr_noop("Last"),
            Self::Any => tr_noop("Any"),
            Self::Other => tr_noop("Other"),
        }
    }
}

/// Follow action of a clip or a scene: after `bars` bars, action A with
/// `chance_a` percent, action B otherwise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Follow {
    /// 0: no follow action.
    pub bars: u16,
    pub a: FollowAction,
    pub b: FollowAction,
    pub chance_a: u8,
}

impl Follow {
    pub const OFF: Follow = Follow { bars: 0, a: FollowAction::Next, b: FollowAction::Any, chance_a: 100 };
    /// Longest wait, in bars.
    pub const MAX_BARS: u16 = 64;

    pub fn is_on(&self) -> bool {
        self.bars > 0
    }

    /// A or B, drawn from `seed`: the same seed always picks the same.
    pub fn pick(&self, seed: u64) -> FollowAction {
        if splitmix(seed) % 100 < u64::from(self.chance_a.min(100)) { self.a } else { self.b }
    }
}

impl Default for Follow {
    fn default() -> Self {
        Self::OFF
    }
}

/// Where a follow action leads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// Nothing changes.
    Keep,
    Stop,
    Play(usize),
}

/// Resolves `action` from row `current` among the `filled` rows; `seed`
/// draws the random ones.
pub fn resolve(action: FollowAction, current: usize, filled: &[bool], seed: u64) -> Target {
    let rows: Vec<usize> = (0..filled.len()).filter(|&r| filled[r]).collect();
    let (Some(&first), Some(&last)) = (rows.first(), rows.last()) else { return Target::Stop };
    let draw = |list: &[usize]| list[(splitmix(seed ^ 0x5eed_f011_0a11) % list.len() as u64) as usize];
    match action {
        FollowAction::None => Target::Keep,
        FollowAction::Stop => Target::Stop,
        FollowAction::Again => Target::Play(current),
        FollowAction::Next => Target::Play(rows.iter().copied().find(|&r| r > current).unwrap_or(first)),
        FollowAction::Previous => Target::Play(rows.iter().copied().rfind(|&r| r < current).unwrap_or(last)),
        FollowAction::First => Target::Play(first),
        FollowAction::Last => Target::Play(last),
        FollowAction::Any => Target::Play(draw(&rows)),
        FollowAction::Other => {
            let others: Vec<usize> = rows.iter().copied().filter(|&r| r != current).collect();
            if others.is_empty() { Target::Keep } else { Target::Play(draw(&others)) }
        }
    }
}

/// Launch settings and content of a session slot (besides its pattern, in
/// [`Project::session_grid`]).
#[derive(Clone, Debug, PartialEq)]
pub struct SlotDef {
    pub launch: LaunchMode,
    /// `None`: the session's quantization.
    pub quant: Option<Quant>,
    /// The clip takes over the position of the one it replaces.
    pub legato: bool,
    pub follow: Follow,
    /// 0: the color of the pattern (or of audio clips).
    pub color: u32,
    /// An audio clip in the slot: its file, loop length (`len`), warp and
    /// gain; `start` and `lane` are unused. Plays on its `track`.
    pub audio: Option<AudioClipDef>,
    /// Launch velocity sensitivity, in percent: at 100, a pad hit softly
    /// plays the clip that much softer; at 0, every launch plays as written.
    pub velocity: u8,
}

/// Velocity taken off a clip (percent, see [`SessionSlot::soft`]) launched
/// with `velocity` (1..=127) on a slot of sensitivity `amount` (percent).
pub fn launch_soft(amount: u8, velocity: u8) -> u8 {
    let amount = f32::from(amount.min(100)) / 100.0;
    let v = f32::from(velocity.clamp(1, 127)) / 127.0;
    (amount * (1.0 - v) * 100.0).round() as u8
}

impl SlotDef {
    pub const DEFAULT: SlotDef = SlotDef {
        launch: LaunchMode::Trigger,
        quant: None,
        legato: false,
        follow: Follow::OFF,
        color: 0,
        audio: None,
        velocity: 0,
    };

    pub fn is_default(&self) -> bool {
        *self == Self::DEFAULT
    }
}

impl Default for SlotDef {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Settings of a scene (a row of slots).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SceneDef {
    pub follow: Follow,
    /// Tempo the scene sets when it starts, in hundredths of BPM (0: none).
    pub tempo: u32,
    /// Time signature the scene sets when it starts.
    pub sig: Option<(u8, u8)>,
}

impl SceneDef {
    /// Tempo range a scene can set.
    pub const BPM: (f32, f32) = (20.0, 999.0);

    /// The scene's tempo in BPM, if it sets one.
    pub fn bpm(&self) -> Option<f32> {
        (self.tempo > 0).then(|| (self.tempo as f32 / 100.0).clamp(Self::BPM.0, Self::BPM.1))
    }

    pub fn set_bpm(&mut self, bpm: Option<f32>) {
        self.tempo = bpm.filter(|b| b.is_finite() && *b > 0.0).map_or(0, |b| (b.clamp(Self::BPM.0, Self::BPM.1) * 100.0).round() as u32);
    }
}

/// A clip playing (or about to) in the session: slot, start and end in
/// ticks (`end` = `u32::MAX` until something replaces it), legato offset and
/// repeat period, as for [`SessionSlot`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LiveClip {
    pub col: usize,
    pub row: usize,
    pub start: u32,
    pub end: u32,
    pub offset: u32,
    pub period: u32,
    /// Velocity taken off (percent): a soft launch of a velocity-sensitive slot.
    pub soft: u8,
}

impl Project {
    /// Settings of a slot (defaults outside the grid).
    pub fn session_slot(&self, col: usize, row: usize) -> &SlotDef {
        self.session_slots.get(col).and_then(|c| c.get(row)).unwrap_or(&SlotDef::DEFAULT)
    }

    pub fn session_slot_mut(&mut self, col: usize, row: usize) -> Option<&mut SlotDef> {
        self.session_slots.get_mut(col)?.get_mut(row)
    }

    /// Pattern of a slot.
    pub fn session_pattern(&self, col: usize, row: usize) -> Option<usize> {
        self.session_grid.get(col)?.get(row).copied().flatten().filter(|&p| p < self.patterns.len())
    }

    /// True if the slot holds a pattern or an audio clip.
    pub fn session_filled(&self, col: usize, row: usize) -> bool {
        self.session_pattern(col, row).is_some() || self.session_slot(col, row).audio.is_some()
    }

    /// Rows of a column that hold something.
    pub fn session_filled_rows(&self, col: usize) -> [bool; SESSION_SCENES] {
        std::array::from_fn(|r| self.session_filled(col, r))
    }

    /// Scenes with at least one clip.
    pub fn session_filled_scenes(&self) -> [bool; SESSION_SCENES] {
        std::array::from_fn(|r| (0..SESSION_COLUMNS).any(|c| self.session_filled(c, r)))
    }

    /// Loop length of a slot's clip, in ticks.
    pub fn session_len(&self, col: usize, row: usize) -> u32 {
        if let Some(p) = self.session_pattern(col, row) {
            return self.patterns[p].len.max(1);
        }
        self.session_slot(col, row).audio.as_ref().map_or(TICKS_PER_BAR, |a| a.len.max(1))
    }

    /// Quantization of a slot's launches.
    pub fn slot_quant(&self, col: usize, row: usize) -> Quant {
        self.session_slot(col, row).quant.unwrap_or(self.session_quant)
    }

    /// Bar of the session, in ticks: the song's first time signature.
    pub fn session_bar(&self) -> u32 {
        self.tempo_map().sig_at(0.0).bar_len()
    }

    /// Ticks of a quantization step in the session's bars.
    pub fn quant_ticks(&self, q: Quant) -> u32 {
        q.ticks_in(self.session_bar())
    }

    /// Tick where a launch made at `pos` with `quant` starts.
    pub fn launch_at(&self, pos: f64, quant: Quant, playing: bool) -> u32 {
        launch_tick_in(pos, self.quant_ticks(quant), playing)
    }

    /// Puts a pattern in a slot (its audio clip goes).
    pub fn set_session_pattern(&mut self, col: usize, row: usize, pattern: usize) {
        if let Some(cell) = self.session_grid.get_mut(col).and_then(|c| c.get_mut(row)) {
            *cell = Some(pattern);
        }
        if let Some(slot) = self.session_slot_mut(col, row) {
            slot.audio = None;
        }
    }

    /// Puts an audio clip in a slot (its pattern goes).
    pub fn set_session_audio(&mut self, col: usize, row: usize, clip: AudioClipDef) {
        if let Some(cell) = self.session_grid.get_mut(col).and_then(|c| c.get_mut(row)) {
            *cell = None;
        }
        if let Some(slot) = self.session_slot_mut(col, row) {
            slot.audio = Some(clip);
        }
    }

    /// Empties a slot, settings included.
    pub fn clear_session_slot(&mut self, col: usize, row: usize) {
        if let Some(cell) = self.session_grid.get_mut(col).and_then(|c| c.get_mut(row)) {
            *cell = None;
        }
        if let Some(slot) = self.session_slot_mut(col, row) {
            *slot = SlotDef::DEFAULT;
        }
    }

    /// Tempo of the session: the song's first tempo, all along.
    pub fn session_tempo(&self) -> TempoMap {
        TempoMap::constant(self.tempo_map().first_bpm() as f32)
    }

    /// Engine slots of the launched pattern clips.
    pub(crate) fn session_slots_live(&self) -> Vec<SessionSlot> {
        self.session_live
            .iter()
            .filter_map(|l| {
                let pattern = self.session_pattern(l.col, l.row)?;
                Some(SessionSlot { pattern, start: l.start, end: l.end, offset: l.offset, period: l.period, soft: l.soft })
            })
            .chain(self.perf_slots())
            .collect()
    }

    /// Cycle (ticks) of a launched audio clip and the slot definition its
    /// warp render is made for.
    fn session_audio_cycle(def: &AudioClipDef, period: u32) -> (u32, AudioClipDef) {
        let len = def.len.max(1);
        let cycle = if period > 0 { period.min(len) } else { len };
        (cycle, AudioClipDef { start: 0, len: cycle, ..def.clone() })
    }

    /// Engine clips of the launched audio clips.
    pub(crate) fn session_audio_live(&self, sample_rate: f32, loader: SampleLoader, warps: WarpLookup) -> Vec<AudioClip> {
        let tempo = self.session_tempo();
        let mut clips: Vec<AudioClip> = self
            .session_live
            .iter()
            .filter(|l| self.session_pattern(l.col, l.row).is_none())
            .filter_map(|l| {
                let def = self.session_slot(l.col, l.row).audio.as_ref()?;
                let sample: Arc<Sample> = loader(&def.path)?;
                let (cycle, job_def) = Self::session_audio_cycle(def, l.period);
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
                    gain: def.gain * (1.0 - f32::from(l.soft.min(100)) / 100.0),
                    fade_in: def.fade_in,
                    fade_out: def.fade_out,
                    fade_in_curve: def.fade_in_curve,
                    fade_out_curve: def.fade_out_curve,
                    reverse: def.reverse,
                    looped: def.looped,
                    warp,
                    cycle,
                    phase: l.offset % cycle,
                    xfade_in: 0.0,
                    xfade_out: 0.0,
                })
            })
            .collect();
        clips.extend(self.perf_audio(sample_rate, loader, warps));
        clips
    }

    /// Renders needed by the warped audio clips of the session (one loop of
    /// each, at the session tempo).
    pub fn session_warp_jobs(&self, sample_rate: f32, loader: SampleLoader) -> Vec<crate::warp::WarpJob> {
        let tempo = self.session_tempo();
        let mut jobs: Vec<crate::warp::WarpJob> = Vec::new();
        let slots = self.session_slots.iter().flatten().filter_map(|s| s.audio.as_ref().map(|a| (a, 0)));
        let live = self.session_live.iter().filter_map(|l| self.session_slot(l.col, l.row).audio.as_ref().map(|a| (a, l.period)));
        // The performance zone's audio clips loop at the session tempo too.
        let perf = self.perf_audio_defs().into_iter().map(|a| (a, 0));
        for (def, period) in slots.chain(live).chain(perf) {
            let Some(sample) = loader(&def.path).map(|s| self.tuned_sample(&def.path, s)) else { continue };
            let (_, job_def) = Self::session_audio_cycle(def, period);
            if let Some(job) = crate::warp::WarpJob::for_clip(&job_def, &sample, &tempo, sample_rate)
                && !jobs.iter().any(|j| j.key == job.key)
            {
                jobs.push(job);
            }
        }
        jobs
    }

    /// Writes recorded session takes into the playlist, at the ticks they
    /// played: pattern clips (repeats split per period) and audio clips (one
    /// per loop, so that each starts where the session played it), on the
    /// lane of their column. Returns how many clips were written.
    pub fn write_takes(&mut self, takes: &[Take]) -> usize {
        let tempo = self.session_tempo();
        let before = self.clips.len() + self.audio_clips.len();
        for t in takes {
            if t.end <= t.start {
                continue;
            }
            let live = SessionSlot { pattern: 0, start: t.start, end: t.end, offset: t.offset, period: t.period, soft: 0 };
            // Pieces: one per repeat, else the whole take.
            let mut pieces = Vec::new();
            if t.period > 0 {
                let mut s = t.start;
                while s < t.end {
                    let e = (s.saturating_add(t.period)).min(t.end);
                    pieces.push((s, e));
                    s = e;
                }
            } else {
                pieces.push((t.start, t.end));
            }
            if let Some(pattern) = self.session_pattern(t.col, t.row) {
                let len = self.patterns[pattern].len.max(1);
                for (s, e) in pieces {
                    let offset = (live.local(s as f64) as u32) % len;
                    self.clips.push(ClipDef { pattern, lane: t.col, start: s, len: e - s, offset, muted: false });
                }
            } else if let Some(def) = self.session_slot(t.col, t.row).audio.clone() {
                let (cycle, _) = Self::session_audio_cycle(&def, t.period);
                let base = AudioClipDef { start: 0, ..def.clone() };
                for (s, e) in pieces {
                    // Cut at each restart of the loop.
                    let mut a = s;
                    while a < e {
                        let phase = live.local(a as f64) as u32 % cycle;
                        let b = (a + (cycle - phase)).min(e);
                        self.audio_clips.push(AudioClipDef {
                            lane: t.col,
                            start: a,
                            len: b - a,
                            offset: base.offset_at(phase as f64, &tempo),
                            ..def.clone()
                        });
                        a = b;
                    }
                }
            }
        }
        self.clips.len() + self.audio_clips.len() - before
    }
}

// --- Player ---

/// What a column does at some tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Play { row: usize, offset: u32, period: u32 },
    Stop,
}

/// A step planned for later.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pending {
    pub at: u32,
    pub step: Step,
    /// Planned by a follow action (not shown as queued).
    pub follow: bool,
}

/// The clip a column plays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Playing {
    pub row: usize,
    pub start: u32,
    pub offset: u32,
    pub period: u32,
}

impl Playing {
    fn slot(&self) -> SessionSlot {
        SessionSlot { pattern: 0, start: self.start, end: u32::MAX, offset: self.offset, period: self.period, soft: 0 }
    }
}

/// One column of the player.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Column {
    pub playing: Option<Playing>,
    /// Sorted by tick.
    pub pending: Vec<Pending>,
    /// Row whose button is held (gate and repeat slots).
    pub held: Option<usize>,
    /// Velocity taken off each row's clip by its last launch (percent).
    pub soft: [u8; SESSION_SCENES],
}

impl Column {
    /// The clip the column plays just before `at`, once the steps planned
    /// before it have happened.
    fn before(&self, at: u32) -> Option<Playing> {
        let mut cur = self.playing;
        for p in self.pending.iter().filter(|p| p.at < at) {
            cur = match p.step {
                Step::Play { row, offset, period } => Some(Playing { row, start: p.at, offset, period }),
                Step::Stop => None,
            };
        }
        cur
    }

    /// The row the column ends up playing, after every planned step.
    pub fn last_row(&self) -> Option<usize> {
        self.before(u32::MAX).map(|p| p.row)
    }

    fn insert(&mut self, p: Pending) {
        let i = self.pending.partition_point(|x| x.at <= p.at);
        self.pending.insert(i, p);
    }

    /// The launch waiting to start, as shown (row, tick).
    pub fn queued(&self) -> Option<(usize, u32)> {
        self.pending.iter().filter(|p| !p.follow).find_map(|p| match p.step {
            Step::Play { row, .. } => Some((row, p.at)),
            Step::Stop => None,
        })
    }

    /// A stop is waiting.
    pub fn stopping(&self) -> bool {
        self.pending.iter().any(|p| !p.follow && p.step == Step::Stop)
    }
}

/// A clip as it played, for the arrangement: slot, ticks, offset and period.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Take {
    pub col: usize,
    pub row: usize,
    pub start: u32,
    pub end: u32,
    pub offset: u32,
    pub period: u32,
}

/// Arrangement recording: the session performance from `from` on.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Recorder {
    pub from: u32,
    pub takes: Vec<Take>,
}

/// The session's playing state (not saved).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Player {
    pub cols: [Column; SESSION_COLUMNS],
    /// The scene launched last and its start.
    pub scene: Option<(usize, u32)>,
    /// Scene launches planned (`None`: stop all), sorted by tick.
    scene_pending: Vec<(u32, Option<usize>)>,
    /// Scene launch waiting to start, as shown.
    pub scene_queued: Option<(usize, u32)>,
    /// Seed of the follow actions' chances.
    pub seed: u64,
    pub recorder: Option<Recorder>,
    /// Scenes that started since the interface last looked (their tempo
    /// and time signature apply then).
    pub started: Vec<usize>,
}

impl Player {
    pub fn new(seed: u64) -> Self {
        Self { seed, ..Self::default() }
    }

    /// True if nothing plays or waits.
    pub fn is_idle(&self) -> bool {
        self.cols.iter().all(|c| c.playing.is_none() && c.pending.is_empty())
    }

    /// Offset of a legato launch of `row` at `at`: the position of the clip
    /// it replaces, in the new clip's loop.
    fn legato_offset(&self, p: &Project, col: usize, row: usize, at: u32) -> u32 {
        if !p.session_slot(col, row).legato {
            return 0;
        }
        let Some(prev) = self.cols[col].before(at) else { return 0 };
        let pos = prev.slot().local(at as f64) as u32 % p.session_len(col, prev.row);
        pos % p.session_len(col, row)
    }

    /// Plans a launch made by hand: other launches waiting go, follow
    /// actions due earlier stay.
    fn schedule(&mut self, col: usize, at: u32, step: Step) {
        let c = &mut self.cols[col];
        c.pending.retain(|x| x.follow && x.at < at);
        c.insert(Pending { at, step, follow: false });
    }

    /// The slot's button is pressed (mouse, key or note) at `pos`.
    pub fn launch(&mut self, p: &Project, col: usize, row: usize, pos: f64, playing: bool) {
        self.launch_with(p, col, row, pos, playing, 127);
    }

    /// Like [`Player::launch`], hit with `velocity` (a pad): a velocity-sensitive
    /// slot plays softer.
    pub fn launch_with(&mut self, p: &Project, col: usize, row: usize, pos: f64, playing: bool, velocity: u8) {
        if col >= SESSION_COLUMNS || !p.session_filled(col, row) {
            return;
        }
        let slot = p.session_slot(col, row);
        self.cols[col].soft[row] = launch_soft(slot.velocity, velocity);
        let quant = p.slot_quant(col, row);
        let at = p.launch_at(pos, quant, playing);
        if slot.launch == LaunchMode::Toggle && self.cols[col].last_row() == Some(row) {
            self.schedule(col, at, Step::Stop);
            return;
        }
        let period = if slot.launch == LaunchMode::Repeat { p.quant_ticks(quant).max(TICKS_PER_STEP) } else { 0 };
        if slot.launch.held() {
            self.cols[col].held = Some(row);
        }
        // The launch replaces the one waiting: legato follows what plays before.
        self.cols[col].pending.retain(|x| x.follow && x.at < at);
        let offset = self.legato_offset(p, col, row, at);
        self.schedule(col, at, Step::Play { row, offset, period });
    }

    /// The slot's button is released at `pos`: a gate clip stops on the next
    /// boundary (after one quantum at least), a repeating clip plays on.
    pub fn release(&mut self, p: &Project, col: usize, row: usize, pos: f64, playing: bool) {
        if col >= SESSION_COLUMNS || self.cols[col].held != Some(row) {
            return;
        }
        self.cols[col].held = None;
        let quant = p.slot_quant(col, row);
        match p.session_slot(col, row).launch {
            LaunchMode::Gate => {
                let started = self.started_at(col, row);
                let at = p.launch_at(pos, quant, playing).max(started.map_or(0, |s| s + p.quant_ticks(quant).max(TICKS_PER_STEP)));
                if playing && started.is_some() {
                    let c = &mut self.cols[col];
                    c.pending.retain(|x| x.at < at);
                    c.insert(Pending { at, step: Step::Stop, follow: false });
                } else {
                    self.cols[col] = Column::default();
                }
            }
            LaunchMode::Repeat => {
                let now = pos.max(0.0) as u32;
                let c = &mut self.cols[col];
                // Waiting: it will just play.
                for x in c.pending.iter_mut() {
                    if let Step::Play { row: r, period, .. } = &mut x.step
                        && *r == row
                    {
                        *period = 0;
                    }
                }
                if let Some(pl) = c.playing.filter(|pl| pl.row == row && pl.period > 0) {
                    // From its last restart on, the clip simply plays on.
                    let last = pl.start + now.saturating_sub(pl.start) / pl.period * pl.period;
                    self.close_take(col, last);
                    let c = &mut self.cols[col];
                    c.playing = Some(Playing { start: last, period: 0, ..pl });
                    self.open_take(col, last);
                }
            }
            LaunchMode::Trigger | LaunchMode::Toggle => {}
        }
    }

    /// Start of the clip of `row` in the column (playing or waiting).
    fn started_at(&self, col: usize, row: usize) -> Option<u32> {
        let c = &self.cols[col];
        c.pending
            .iter()
            .rev()
            .find_map(|x| matches!(x.step, Step::Play { row: r, .. } if r == row).then_some(x.at))
            .or_else(|| c.playing.filter(|pl| pl.row == row).map(|pl| pl.start))
    }

    /// Launches a whole row on the session's quantization: empty slots stop
    /// their column.
    pub fn launch_scene(&mut self, p: &Project, row: usize, pos: f64, playing: bool) {
        if row >= SESSION_SCENES {
            return;
        }
        let at = p.launch_at(pos, p.session_quant, playing);
        self.plan_scene(p, row, at, false);
        self.scene_pending.retain(|&(t, _)| t < at);
        self.scene_pending.push((at, Some(row)));
        self.scene_queued = Some((row, at));
    }

    fn plan_scene(&mut self, p: &Project, row: usize, at: u32, follow: bool) {
        for col in 0..SESSION_COLUMNS {
            if !follow {
                self.cols[col].pending.retain(|x| x.follow && x.at < at);
            }
            let step = if p.session_filled(col, row) {
                let offset = self.legato_offset(p, col, row, at);
                Step::Play { row, offset, period: 0 }
            } else {
                Step::Stop
            };
            let c = &mut self.cols[col];
            if follow {
                c.pending.retain(|x| x.at < at);
                c.insert(Pending { at, step, follow: true });
            } else {
                self.schedule(col, at, step);
            }
        }
    }

    /// Keys and notes held on a scene: its gate and repeat slots let go.
    pub fn release_scene(&mut self, p: &Project, row: usize, pos: f64, playing: bool) {
        for col in 0..SESSION_COLUMNS {
            self.release(p, col, row, pos, playing);
        }
    }

    /// Stops a column on the session's quantization (right away when the
    /// transport is stopped).
    pub fn stop_column(&mut self, p: &Project, col: usize, pos: f64, playing: bool) {
        if col >= SESSION_COLUMNS {
            return;
        }
        if !playing {
            self.cols[col] = Column::default();
            return;
        }
        let at = p.launch_at(pos, p.session_quant, playing);
        self.cols[col].held = None;
        self.schedule(col, at, Step::Stop);
    }

    /// Stops everything on the session's quantization.
    pub fn stop_all(&mut self, p: &Project, pos: f64, playing: bool) {
        for col in 0..SESSION_COLUMNS {
            self.stop_column(p, col, pos, playing);
        }
        self.scene_pending.clear();
        self.scene_queued = None;
        if !playing {
            self.scene = None;
        }
    }

    /// Everything stops now (transport stopped, mode left).
    pub fn reset(&mut self) {
        let (seed, recorder) = (self.seed, self.recorder.take());
        *self = Self { seed, recorder, ..Self::default() };
    }

    /// Steps whose tick has come happen; the clips that start draw their
    /// follow action. True if anything changed.
    pub fn advance(&mut self, p: &Project, pos: f64) -> bool {
        let mut changed = false;
        let now = pos.max(0.0);
        // Scenes first: a scene's follow action plans its columns.
        while let Some(&(at, scene)) = self.scene_pending.first()
            && at as f64 <= now
        {
            self.scene_pending.remove(0);
            changed = true;
            match scene {
                Some(row) => {
                    self.scene = Some((row, at));
                    self.started.push(row);
                    if self.scene_queued.is_some_and(|(r, t)| r == row && t == at) {
                        self.scene_queued = None;
                    }
                    self.scene_follow(p, row, at);
                }
                None => self.scene = None,
            }
        }
        for col in 0..SESSION_COLUMNS {
            while let Some(&x) = self.cols[col].pending.first()
                && x.at as f64 <= now
            {
                self.cols[col].pending.remove(0);
                changed = true;
                self.close_take(col, x.at);
                match x.step {
                    Step::Play { row, offset, period } => {
                        self.cols[col].playing = Some(Playing { row, start: x.at, offset, period });
                        self.open_take(col, x.at);
                        self.clip_follow(p, col, row, x.at);
                    }
                    Step::Stop => {
                        let c = &mut self.cols[col];
                        c.playing = None;
                        c.held = None;
                    }
                }
            }
        }
        changed
    }

    fn seed_of(&self, a: usize, b: usize, at: u32) -> u64 {
        splitmix(self.seed ^ (a as u64) << 48 ^ (b as u64) << 36 ^ u64::from(at))
    }

    /// Plans the follow action of a clip that starts at `at`.
    fn clip_follow(&mut self, p: &Project, col: usize, row: usize, at: u32) {
        let follow = p.session_slot(col, row).follow;
        if !follow.is_on() || self.cols[col].held == Some(row) {
            return;
        }
        let t = at.saturating_add(u32::from(follow.bars) * p.session_bar());
        // Something planned earlier already replaces the clip.
        if self.cols[col].pending.iter().any(|x| x.at <= t) {
            return;
        }
        let seed = self.seed_of(col, row, at);
        let step = match resolve(follow.pick(seed), row, &p.session_filled_rows(col), seed) {
            Target::Keep => return,
            Target::Stop => Step::Stop,
            Target::Play(r) => Step::Play { row: r, offset: self.legato_offset(p, col, r, t), period: 0 },
        };
        self.cols[col].insert(Pending { at: t, step, follow: true });
    }

    /// Plans the follow action of a scene that starts at `at`.
    fn scene_follow(&mut self, p: &Project, row: usize, at: u32) {
        let follow = p.session_scenes.get(row).copied().unwrap_or_default().follow;
        if !follow.is_on() {
            return;
        }
        let t = at.saturating_add(u32::from(follow.bars) * p.session_bar());
        let seed = self.seed_of(SESSION_COLUMNS, row, at);
        match resolve(follow.pick(seed), row, &p.session_filled_scenes(), seed) {
            Target::Keep => {}
            Target::Stop => {
                for c in &mut self.cols {
                    c.pending.retain(|x| x.at < t);
                    c.insert(Pending { at: t, step: Step::Stop, follow: true });
                }
                self.scene_pending.retain(|&(x, _)| x < t);
                self.scene_pending.push((t, None));
            }
            Target::Play(r) => {
                self.plan_scene(p, r, t, true);
                self.scene_pending.retain(|&(x, _)| x < t);
                self.scene_pending.push((t, Some(r)));
            }
        }
    }

    /// The clips for the engine: each playing or planned clip, until the next
    /// step of its column.
    pub fn live(&self, p: &Project) -> Vec<LiveClip> {
        let mut out = Vec::new();
        for (col, c) in self.cols.iter().enumerate() {
            let mut cur = c.playing;
            for x in &c.pending {
                if let Some(pl) = cur {
                    out.push(LiveClip { col, row: pl.row, start: pl.start, end: x.at.max(pl.start), offset: pl.offset, period: pl.period, soft: c.soft[pl.row] });
                }
                cur = match x.step {
                    Step::Play { row, offset, period } => Some(Playing { row, start: x.at, offset, period }),
                    Step::Stop => None,
                };
            }
            if let Some(pl) = cur {
                out.push(LiveClip { col, row: pl.row, start: pl.start, end: u32::MAX, offset: pl.offset, period: pl.period, soft: c.soft[pl.row] });
            }
        }
        out.retain(|l| l.end > l.start && p.session_filled(l.col, l.row));
        out
    }

    // --- Arrangement recording ---

    /// Starts recording the performance at `pos`.
    pub fn start_recording(&mut self, pos: f64) {
        let from = pos.max(0.0) as u32;
        self.recorder = Some(Recorder { from, takes: Vec::new() });
        for col in 0..SESSION_COLUMNS {
            self.open_take(col, from);
        }
    }

    /// Stops recording at `pos`: the takes, clips still playing cut there.
    pub fn finish_recording(&mut self, pos: f64) -> Vec<Take> {
        let end = pos.max(0.0) as u32;
        for col in 0..SESSION_COLUMNS {
            self.close_take(col, end);
        }
        self.recorder.take().map(|r| r.takes).unwrap_or_default()
    }

    /// A take opens on what the column plays from `at` (an open take has
    /// `end` = `u32::MAX`).
    fn open_take(&mut self, col: usize, at: u32) {
        let Some(pl) = self.cols[col].playing else { return };
        let Some(rec) = &mut self.recorder else { return };
        let start = at.max(rec.from).max(pl.start);
        // Where the clip is at the take's start.
        let offset = pl.slot().local(start as f64) as u32;
        let (offset, period, start) = if pl.period > 0 {
            // Repeats: the take keeps the clip's own start for its periods.
            (pl.offset, pl.period, pl.start.max(rec.from))
        } else {
            (offset, 0, start)
        };
        rec.takes.push(Take { col, row: pl.row, start, end: u32::MAX, offset, period });
    }

    /// The open take of the column ends at `at`.
    fn close_take(&mut self, col: usize, at: u32) {
        let Some(rec) = &mut self.recorder else { return };
        if let Some(t) = rec.takes.iter_mut().rev().find(|t| t.col == col && t.end == u32::MAX) {
            t.end = at.max(t.start);
        }
        rec.takes.retain(|t| t.end > t.start);
    }
}

// --- JSON ---

fn obj(pairs: Vec<(&str, Json)>) -> Json {
    Json::Obj(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

fn follow_json(f: &Follow) -> Json {
    obj(vec![("bars", f.bars.into()), ("a", f.a.id().into()), ("b", f.b.id().into()), ("chance_a", f.chance_a.into())])
}

fn load_follow(v: Option<&Json>) -> Follow {
    let Some(v) = v else { return Follow::OFF };
    let action = |k: &str, d| v.get(k).and_then(Json::as_str).and_then(FollowAction::from_id).unwrap_or(d);
    let num = |k: &str| v.get(k).and_then(Json::as_f64).filter(|n| n.is_finite());
    Follow {
        bars: num("bars").map_or(0, |n| n.round().clamp(0.0, Follow::MAX_BARS as f64) as u16),
        a: action("a", Follow::OFF.a),
        b: action("b", Follow::OFF.b),
        chance_a: num("chance_a").map_or(100, |n| n.round().clamp(0.0, 100.0) as u8),
    }
}

/// Session settings for the project file: slots and scenes off their
/// defaults, and the quantization.
pub(crate) fn session_json(p: &Project, audio_json: fn(&AudioClipDef) -> Json) -> Vec<(&'static str, Json)> {
    let mut slots = Vec::new();
    for (c, col) in p.session_slots.iter().enumerate() {
        for (r, s) in col.iter().enumerate().filter(|(_, s)| !s.is_default()) {
            let mut pairs = vec![("col", c.into()), ("row", r.into()), ("launch", s.launch.id().into())];
            if let Some(q) = s.quant {
                pairs.push(("quant", q.id().into()));
            }
            if s.legato {
                pairs.push(("legato", true.into()));
            }
            if s.follow != Follow::OFF {
                pairs.push(("follow", follow_json(&s.follow)));
            }
            if s.color != 0 {
                pairs.push(("color", s.color.into()));
            }
            if s.velocity > 0 {
                pairs.push(("velocity", s.velocity.into()));
            }
            if let Some(a) = &s.audio {
                pairs.push(("audio", audio_json(a)));
            }
            slots.push(obj(pairs));
        }
    }
    let scenes = p
        .session_scenes
        .iter()
        .enumerate()
        .filter(|(_, s)| **s != SceneDef::default())
        .map(|(r, s)| {
            let mut pairs = vec![("row", r.into())];
            if s.follow != Follow::OFF {
                pairs.push(("follow", follow_json(&s.follow)));
            }
            if let Some(bpm) = s.bpm() {
                pairs.push(("tempo", bpm.into()));
            }
            if let Some((num, den)) = s.sig {
                pairs.push(("sig", Json::Arr(vec![num.into(), den.into()])));
            }
            obj(pairs)
        })
        .collect();
    vec![("session_quant", p.session_quant.id().into()), ("session_slots", Json::Arr(slots)), ("session_scenes", Json::Arr(scenes))]
}

/// Reads what [`session_json`] wrote; out of range or malformed entries are skipped.
pub(crate) fn load_session(p: &mut Project, root: &Json, load_audio: fn(&Json) -> Option<AudioClipDef>) {
    let idx = |v: Option<&Json>, limit: usize| {
        v.and_then(Json::as_f64).filter(|n| *n >= 0.0 && n.fract() == 0.0 && *n < limit as f64).map(|n| n as usize)
    };
    p.session_quant = root.get("session_quant").and_then(Json::as_str).and_then(Quant::from_id).unwrap_or_default();
    for v in root.get("session_slots").and_then(Json::as_arr).unwrap_or(&[]) {
        let (Some(c), Some(r)) = (idx(v.get("col"), SESSION_COLUMNS), idx(v.get("row"), SESSION_SCENES)) else { continue };
        let audio = v.get("audio").and_then(load_audio);
        let slot = SlotDef {
            launch: v.get("launch").and_then(Json::as_str).and_then(LaunchMode::from_id).unwrap_or_default(),
            quant: v.get("quant").and_then(Json::as_str).and_then(Quant::from_id),
            legato: v.get("legato").and_then(Json::as_bool).unwrap_or(false),
            follow: load_follow(v.get("follow")),
            color: v.get("color").and_then(Json::as_f64).filter(|n| n.is_finite() && *n >= 0.0).map_or(0, |n| n as u32 & 0xff_ffff),
            audio,
            velocity: v.get("velocity").and_then(Json::as_f64).filter(|n| n.is_finite()).map_or(0, |n| n.round().clamp(0.0, 100.0) as u8),
        };
        // A slot holds a pattern or an audio clip, not both.
        if slot.audio.is_some() {
            p.session_grid[c][r] = None;
        }
        p.session_slots[c][r] = slot;
    }
    for v in root.get("session_scenes").and_then(Json::as_arr).unwrap_or(&[]) {
        if let Some(r) = idx(v.get("row"), SESSION_SCENES) {
            let scene = &mut p.session_scenes[r];
            scene.follow = load_follow(v.get("follow"));
            scene.set_bpm(v.get("tempo").and_then(Json::as_f64).map(|b| b as f32));
            scene.sig = v.get("sig").and_then(Json::as_arr).and_then(|a| {
                let n = |k: usize| a.get(k).and_then(Json::as_f64).filter(|n| n.fract() == 0.0);
                let (num, den) = (n(0)?, n(1)?);
                ((1.0..=32.0).contains(&num) && [1.0, 2.0, 4.0, 8.0, 16.0].contains(&den)).then_some((num as u8, den as u8))
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ponzik_engine::PPQ;

    const BAR: u32 = TICKS_PER_BAR;

    /// Three patterns of one bar in column 0, rows 0, 1 and 3; one of two bars in column 1, row 0.
    fn project() -> Project {
        let mut p = Project::empty();
        for _ in 0..3 {
            p.add_pattern();
        }
        p.patterns[3].len = 2 * BAR;
        p.set_session_pattern(0, 0, 0);
        p.set_session_pattern(0, 1, 1);
        p.set_session_pattern(0, 3, 2);
        p.set_session_pattern(1, 0, 3);
        p
    }

    #[test]
    fn launches_land_on_their_quantization() {
        assert_eq!(launch_tick(100.0, Quant::Bar, true), BAR);
        assert_eq!(launch_tick(100.0, Quant::Beat, true), PPQ * 2);
        assert_eq!(launch_tick(100.0, Quant::Sixteenth, true), 120);
        assert_eq!(launch_tick(100.0, Quant::Bars2, true), 2 * BAR);
        assert_eq!(launch_tick(900.0, Quant::Bars4, true), 4 * BAR);
        assert_eq!(launch_tick(100.3, Quant::None, true), 103);
        // Right on a boundary: too late for it, the next one.
        assert_eq!(launch_tick(BAR as f64, Quant::Bar, true), 2 * BAR);
        assert_eq!(launch_tick(BAR as f64 - 1.0, Quant::Bar, true), 2 * BAR);
        assert_eq!(launch_tick(BAR as f64 - 3.0, Quant::Bar, true), BAR);
        // Transport stopped: from the start.
        assert_eq!(launch_tick(5000.0, Quant::Bar, false), 0);

        // Per slot, or the session's.
        let mut p = project();
        p.session_quant = Quant::Beat;
        p.session_slot_mut(0, 1).unwrap().quant = Some(Quant::Bars2);
        let mut pl = Player::new(1);
        pl.launch(&p, 0, 0, 100.0, true);
        assert_eq!(pl.cols[0].queued(), Some((0, 192)));
        pl.launch(&p, 0, 1, 100.0, true);
        assert_eq!(pl.cols[0].queued(), Some((1, 2 * BAR)));
        assert_eq!(pl.cols[0].pending.len(), 1, "a new launch replaces the waiting one");
        assert!(!pl.advance(&p, 700.0));
        assert!(pl.advance(&p, 768.0));
        assert_eq!(pl.cols[0].playing, Some(Playing { row: 1, start: 768, offset: 0, period: 0 }));
        // The engine gets the clip before it starts.
        let mut pl = Player::new(1);
        pl.launch(&p, 0, 0, 100.0, true);
        assert_eq!(pl.live(&p), [LiveClip { col: 0, row: 0, start: 192, end: u32::MAX, offset: 0, period: 0, soft: 0 }]);
    }

    #[test]
    fn follow_actions_pick_with_their_seeded_chance() {
        let f = Follow { bars: 1, a: FollowAction::Next, b: FollowAction::Stop, chance_a: 70 };
        let picks: Vec<FollowAction> = (0..2000).map(|s| f.pick(s)).collect();
        let next = picks.iter().filter(|&&a| a == FollowAction::Next).count();
        assert!((1300..1500).contains(&next), "{next} of 2000");
        // Same seed, same pick.
        assert!((0..100).all(|s| f.pick(s) == f.pick(s)));
        assert!((0..100).all(|s| Follow { chance_a: 100, ..f }.pick(s) == FollowAction::Next));
        assert!((0..100).all(|s| Follow { chance_a: 0, ..f }.pick(s) == FollowAction::Stop));

        let filled = [true, true, false, true, false];
        assert_eq!(resolve(FollowAction::Next, 1, &filled, 0), Target::Play(3));
        assert_eq!(resolve(FollowAction::Next, 3, &filled, 0), Target::Play(0));
        assert_eq!(resolve(FollowAction::Previous, 0, &filled, 0), Target::Play(3));
        assert_eq!(resolve(FollowAction::Previous, 3, &filled, 0), Target::Play(1));
        assert_eq!(resolve(FollowAction::First, 3, &filled, 0), Target::Play(0));
        assert_eq!(resolve(FollowAction::Last, 0, &filled, 0), Target::Play(3));
        assert_eq!(resolve(FollowAction::Again, 1, &filled, 0), Target::Play(1));
        assert_eq!(resolve(FollowAction::Stop, 1, &filled, 0), Target::Stop);
        assert_eq!(resolve(FollowAction::None, 1, &filled, 0), Target::Keep);
        let others: Vec<Target> = (0..200).map(|s| resolve(FollowAction::Other, 1, &filled, s)).collect();
        assert!(others.iter().all(|t| matches!(t, Target::Play(0 | 3))));
        assert!(others.contains(&Target::Play(0)) && others.contains(&Target::Play(3)));
        let any: Vec<Target> = (0..200).map(|s| resolve(FollowAction::Any, 1, &filled, s)).collect();
        assert!([0, 1, 3].iter().all(|r| any.contains(&Target::Play(*r))));
        assert_eq!(resolve(FollowAction::Other, 0, &[true, false], 9), Target::Keep);

        // In the player: a clip follows on after its bars, drawn when it starts.
        let mut p = project();
        p.session_slot_mut(0, 0).unwrap().follow = Follow { bars: 2, a: FollowAction::Next, b: FollowAction::Previous, chance_a: 50 };
        p.session_slot_mut(0, 1).unwrap().follow = Follow { bars: 1, a: FollowAction::Stop, ..Follow::OFF };
        let run = |seed: u64| {
            let mut pl = Player::new(seed);
            pl.launch(&p, 0, 0, 0.0, false);
            pl.advance(&p, 0.0);
            let planned = pl.cols[0].pending.clone();
            assert!(planned.iter().all(|x| x.follow), "not shown as queued");
            assert_eq!(pl.cols[0].queued(), None);
            pl.advance(&p, (2 * BAR) as f64);
            (planned, pl.cols[0].playing.map(|x| x.row))
        };
        let mut seen = Vec::new();
        for seed in 0..40 {
            let (planned, row) = run(seed);
            assert_eq!(run(seed).1, row, "seeded: the same performance");
            assert_eq!(planned.len(), 1);
            assert_eq!(planned[0].at, 2 * BAR);
            seen.push(row);
        }
        assert!(seen.contains(&Some(1)) && seen.contains(&Some(3)), "next and previous both drawn");
        // Row 1 then stops after a bar.
        let seed = (0..40).find(|&s| run(s).1 == Some(1)).unwrap();
        let mut pl = Player::new(seed);
        pl.launch(&p, 0, 0, 0.0, false);
        pl.advance(&p, 0.0);
        pl.advance(&p, (2 * BAR) as f64);
        assert_eq!(
            pl.live(&p),
            [LiveClip { col: 0, row: 1, start: 2 * BAR, end: 3 * BAR, offset: 0, period: 0, soft: 0 }],
            "the stop is planned a bar ahead"
        );
        pl.advance(&p, (3 * BAR) as f64);
        assert!(pl.is_idle());
    }

    #[test]
    fn gate_plays_while_held_and_toggle_stops_on_second_press() {
        let mut p = project();
        p.session_slot_mut(0, 0).unwrap().launch = LaunchMode::Gate;
        let mut pl = Player::new(0);
        pl.launch(&p, 0, 0, 100.0, true);
        pl.advance(&p, 400.0);
        assert_eq!(pl.cols[0].held, Some(0));
        pl.release(&p, 0, 0, 900.0, true);
        assert_eq!(pl.live(&p), [LiveClip { col: 0, row: 0, start: BAR, end: 3 * BAR, offset: 0, period: 0, soft: 0 }]);
        pl.advance(&p, (3 * BAR) as f64);
        assert!(pl.is_idle());
        // A quick tap still plays one quantum.
        let mut pl = Player::new(0);
        pl.launch(&p, 0, 0, 100.0, true);
        pl.release(&p, 0, 0, 120.0, true);
        assert_eq!(pl.live(&p), [LiveClip { col: 0, row: 0, start: BAR, end: 2 * BAR, offset: 0, period: 0, soft: 0 }]);
        // Without quantization, it stops right on release.
        p.session_slot_mut(0, 0).unwrap().quant = Some(Quant::None);
        let mut pl = Player::new(0);
        pl.launch(&p, 0, 0, 100.0, true);
        pl.advance(&p, 150.0);
        pl.release(&p, 0, 0, 400.0, true);
        assert_eq!(pl.live(&p)[0].end, 402);
        // Released while the transport is stopped: nothing left.
        let mut pl = Player::new(0);
        pl.launch(&p, 0, 0, 0.0, false);
        pl.release(&p, 0, 0, 0.0, false);
        assert!(pl.is_idle());

        p.session_slot_mut(0, 1).unwrap().launch = LaunchMode::Toggle;
        let mut pl = Player::new(0);
        pl.launch(&p, 0, 1, 100.0, true);
        pl.advance(&p, 400.0);
        pl.launch(&p, 0, 1, 500.0, true);
        assert!(pl.cols[0].stopping());
        assert_eq!(pl.live(&p), [LiveClip { col: 0, row: 1, start: BAR, end: 2 * BAR, offset: 0, period: 0, soft: 0 }]);
    }

    #[test]
    fn repeat_starts_over_while_held_then_plays_on() {
        let mut p = project();
        let s = p.session_slot_mut(0, 0).unwrap();
        (s.launch, s.quant) = (LaunchMode::Repeat, Some(Quant::Beat));
        let mut pl = Player::new(0);
        pl.launch(&p, 0, 0, 10.0, true);
        pl.advance(&p, 100.0);
        assert_eq!(pl.live(&p), [LiveClip { col: 0, row: 0, start: 96, end: u32::MAX, offset: 0, period: 96, soft: 0 }]);
        pl.release(&p, 0, 0, 250.0, true);
        // From its last restart (tick 192) it plays on.
        assert_eq!(pl.live(&p), [LiveClip { col: 0, row: 0, start: 192, end: u32::MAX, offset: 0, period: 0, soft: 0 }]);
    }

    #[test]
    fn legato_keeps_the_position_of_the_clip_it_replaces() {
        let mut p = project();
        p.session_quant = Quant::Beat;
        p.patterns[1].len = 3 * PPQ;
        p.session_slot_mut(0, 1).unwrap().legato = true;
        let mut pl = Player::new(0);
        pl.launch(&p, 0, 0, 0.0, false);
        pl.advance(&p, 0.0);
        // At tick 672 the first clip (a bar long) is 288 ticks in: 0 in the
        // new clip's 288-tick loop.
        pl.launch(&p, 0, 1, 650.0, true);
        assert_eq!(pl.cols[0].queued(), Some((1, 672)));
        let live = pl.live(&p);
        assert_eq!(live[1], LiveClip { col: 0, row: 1, start: 672, end: u32::MAX, offset: 0, period: 0, soft: 0 });
        pl.launch(&p, 0, 1, 700.0, true);
        assert_eq!(pl.live(&p)[1], LiveClip { col: 0, row: 1, start: 768, end: u32::MAX, offset: 0, period: 0, soft: 0 }, "2 bars: 0 in the bar");
        pl.launch(&p, 0, 1, 520.0, true);
        // 576 is 192 into the bar.
        assert_eq!(pl.live(&p)[1], LiveClip { col: 0, row: 1, start: 576, end: u32::MAX, offset: 192, period: 0, soft: 0 });
        pl.advance(&p, 600.0);
        // Back to the first clip without legato: from its start.
        pl.launch(&p, 0, 0, 650.0, true);
        assert_eq!(pl.live(&p)[1].offset, 0);
        p.session_slot_mut(0, 0).unwrap().legato = true;
        pl.launch(&p, 0, 0, 700.0, true);
        // At 768: 768 − 576 + 192 = 384 → 96 of its 288-tick loop.
        assert_eq!(pl.live(&p)[1], LiveClip { col: 0, row: 0, start: 768, end: u32::MAX, offset: 96, period: 0, soft: 0 });
    }

    #[test]
    fn scenes_launch_rows_and_follow_on() {
        let mut p = project();
        p.set_session_pattern(1, 1, 0);
        p.session_scenes[0].follow = Follow { bars: 4, a: FollowAction::Next, ..Follow::OFF };
        let mut pl = Player::new(0);
        pl.launch_scene(&p, 0, 0.0, false);
        assert_eq!(pl.scene_queued, Some((0, 0)));
        pl.advance(&p, 0.0);
        assert_eq!(pl.scene, Some((0, 0)));
        // Scene 1 is planned four bars on, in both columns; empty slots stop.
        let live = pl.live(&p);
        assert!(live.contains(&LiveClip { col: 0, row: 0, start: 0, end: 4 * BAR, offset: 0, period: 0, soft: 0 }));
        assert!(live.contains(&LiveClip { col: 1, row: 1, start: 4 * BAR, end: u32::MAX, offset: 0, period: 0, soft: 0 }));
        assert!(live.iter().all(|l| l.col < 2));
        pl.advance(&p, (4 * BAR) as f64);
        assert_eq!(pl.scene, Some((1, 4 * BAR)));
        pl.stop_all(&p, (4 * BAR + 10) as f64, true);
        assert!(pl.cols[0].stopping());
    }

    #[test]
    fn arrangement_record_writes_the_performance_into_the_playlist() {
        let mut p = project();
        p.patterns[1].len = 3 * PPQ;
        p.session_slot_mut(0, 1).unwrap().legato = true;
        let mut pl = Player::new(0);
        pl.start_recording(0.0);
        pl.launch(&p, 0, 0, 0.0, false);
        pl.launch(&p, 1, 0, 0.0, false);
        pl.advance(&p, 0.0);
        pl.launch(&p, 0, 1, 1000.0, true);
        pl.advance(&p, 1152.0);
        pl.stop_column(&p, 1, 1200.0, true);
        pl.advance(&p, 1536.0);
        let takes = pl.finish_recording(2000.0);
        assert!(pl.recorder.is_none());
        assert_eq!(
            takes,
            [
                Take { col: 0, row: 0, start: 0, end: 1152, offset: 0, period: 0 },
                Take { col: 1, row: 0, start: 0, end: 1536, offset: 0, period: 0 },
                Take { col: 0, row: 1, start: 1152, end: 2000, offset: 0, period: 0 },
            ]
        );
        let before = p.clips.len();
        assert_eq!(p.write_takes(&takes), 3);
        let clips = &p.clips[before..];
        assert_eq!(clips[0], ClipDef { pattern: 0, lane: 0, start: 0, len: 1152, offset: 0, muted: false });
        assert_eq!(clips[1], ClipDef { pattern: 3, lane: 1, start: 0, len: 1536, offset: 0, muted: false });
        // Legato at 1152 = 3 bars: 0 into the bar.
        assert_eq!(clips[2], ClipDef { pattern: 1, lane: 0, start: 1152, len: 848, offset: 0, muted: false });

        // Recording from mid-clip: the clip's position at the start.
        let mut pl = Player::new(0);
        pl.launch(&p, 1, 0, 0.0, false);
        pl.advance(&p, 0.0);
        pl.start_recording(500.0);
        let takes = pl.finish_recording(900.0);
        assert_eq!(takes, [Take { col: 1, row: 0, start: 500, end: 900, offset: 500, period: 0 }]);
        let n = p.clips.len();
        p.write_takes(&takes);
        assert_eq!(p.clips[n], ClipDef { pattern: 3, lane: 1, start: 500, len: 400, offset: 500, muted: false });

        // Repeats: one clip per restart.
        let s = p.session_slot_mut(0, 0).unwrap();
        (s.launch, s.quant) = (LaunchMode::Repeat, Some(Quant::Beat));
        let mut pl = Player::new(0);
        pl.start_recording(0.0);
        pl.launch(&p, 0, 0, 10.0, true);
        pl.advance(&p, 96.0);
        pl.release(&p, 0, 0, 300.0, true);
        let takes = pl.finish_recording(400.0);
        assert_eq!(takes[0], Take { col: 0, row: 0, start: 96, end: 288, offset: 0, period: 96 });
        let n = p.clips.len();
        p.write_takes(&takes);
        let starts: Vec<(u32, u32)> = p.clips[n..].iter().map(|c| (c.start, c.len)).collect();
        assert_eq!(starts, [(96, 96), (192, 96), (288, 112)]);
    }

    #[test]
    fn audio_takes_are_cut_at_each_loop() {
        let mut p = project();
        let clip = AudioClipDef { path: "loop.wav".into(), len: BAR, track: 3, ..AudioClipDef::default() };
        p.set_session_audio(2, 0, clip);
        assert!(p.session_filled(2, 0));
        assert_eq!(p.session_len(2, 0), BAR);
        let takes = [Take { col: 2, row: 0, start: 200, end: 1000, offset: 200, period: 0 }];
        assert_eq!(p.write_takes(&takes), 3);
        let got: Vec<(u32, u32, usize)> = p.audio_clips.iter().map(|c| (c.start, c.len, c.lane)).collect();
        assert_eq!(got, [(200, 184, 2), (384, 384, 2), (768, 232, 2)]);
        // At 120 BPM a tick is 1/192 s: 200 ticks into the loop.
        assert!((p.audio_clips[0].offset - 200.0 / 192.0).abs() < 1e-9);
        assert_eq!(p.audio_clips[1].offset, 0.0);

        // The engine plays it in session mode, cycling from its phase.
        p.mode = ponzik_engine::PlayMode::Session;
        p.session_live = vec![LiveClip { col: 2, row: 0, start: 384, end: u32::MAX, offset: 100, period: 0, soft: 0 }];
        let sample = Arc::new(Sample { name: "l".into(), sample_rate: 48_000.0, left: vec![0.1; 100], right: vec![0.1; 100], markers: Vec::new() });
        let seq = p.sequence_with(&|_| Some(sample.clone()));
        assert_eq!(seq.audio_clips.len(), 1);
        let a = &seq.audio_clips[0];
        assert_eq!((a.start, a.cycle, a.phase, a.track), (384, BAR, 100, 3));
        assert!(seq.session.is_empty());
    }

    #[test]
    fn velocity_sensitive_slots_play_soft_hits_softer() {
        assert_eq!(launch_soft(0, 1), 0, "not sensitive");
        assert_eq!(launch_soft(100, 127), 0, "a full hit");
        assert_eq!(launch_soft(100, 64), 50);
        assert_eq!(launch_soft(50, 1), 50);
        let mut p = project();
        p.session_slot_mut(0, 0).unwrap().velocity = 100;
        let mut pl = Player::new(0);
        pl.launch_with(&p, 0, 0, 0.0, false, 64);
        pl.launch_with(&p, 1, 0, 0.0, false, 10);
        let live = pl.live(&p);
        assert_eq!(live[0].soft, 50);
        assert_eq!(live[1].soft, 0, "column 1 is not sensitive");
        p.session_live = live;
        p.mode = ponzik_engine::PlayMode::Session;
        let seq = p.sequence_with(&|_| None);
        assert_eq!(seq.session[0].soft, 50);
    }

    #[test]
    fn scenes_set_tempo_and_meter_and_bars_follow_the_meter() {
        let mut p = project();
        p.session_scenes[1].set_bpm(Some(140.5));
        p.session_scenes[1].sig = Some((3, 4));
        assert_eq!(p.session_scenes[1].bpm(), Some(140.5));
        let mut pl = Player::new(0);
        pl.launch_scene(&p, 1, 0.0, false);
        pl.advance(&p, 0.0);
        assert_eq!(pl.started, [1], "the interface applies its tempo and meter");
        // In 3/4, a bar is three beats: launches land there.
        p.signatures = vec![ponzik_engine::TimeSig { tick: 0, num: 3, den: 4 }];
        assert_eq!(p.session_bar(), 3 * PPQ);
        let mut pl = Player::new(0);
        pl.launch(&p, 0, 0, 100.0, true);
        assert_eq!(pl.cols[0].queued(), Some((0, 3 * PPQ)));
        // Saved and read back; junk falls back.
        let back = crate::io::load(&crate::io::save(&p)).unwrap();
        assert_eq!(back.session_scenes[1], p.session_scenes[1]);
        let junk = r#"{"format": "ponzik-project", "session_scenes": [{"row": 0, "tempo": -5, "sig": [3, 5]}, {"row": 1, "tempo": 5000, "sig": [7, 8]}]}"#;
        let p = crate::io::load(junk).unwrap();
        assert_eq!(p.session_scenes[0], SceneDef::default());
        assert_eq!(p.session_scenes[1].bpm(), Some(SceneDef::BPM.1));
        assert_eq!(p.session_scenes[1].sig, Some((7, 8)));
    }

    #[test]
    fn settings_roundtrip_and_tolerate_junk() {
        let mut p = project();
        p.session_quant = Quant::Bars2;
        let s = p.session_slot_mut(0, 1).unwrap();
        s.launch = LaunchMode::Gate;
        s.quant = Some(Quant::Sixteenth);
        s.legato = true;
        s.follow = Follow { bars: 8, a: FollowAction::Other, b: FollowAction::Again, chance_a: 35 };
        s.color = 0x3ccfc4;
        s.velocity = 80;
        p.set_session_audio(4, 2, AudioClipDef { path: "a.wav".into(), len: 768, warp: true, bpm: 128.0, ..Default::default() });
        p.session_scenes[3].follow = Follow { bars: 2, a: FollowAction::Previous, ..Follow::OFF };
        let back = crate::io::load(&crate::io::save(&p)).unwrap();
        assert_eq!(back.session_slots, p.session_slots);
        assert_eq!(back.session_scenes, p.session_scenes);
        assert_eq!(back.session_quant, Quant::Bars2);
        assert_eq!(back.session_grid, p.session_grid);

        let junk = r#"{"format": "ponzik-project", "session_quant": "7",
            "session_slots": [{"col": 99, "row": 0}, {"col": 1, "row": 2, "launch": "wild", "follow": {"bars": 1e9, "a": "x", "chance_a": -4}}, 5],
            "session_scenes": [{"row": -1}, {"row": 2, "follow": "nope"}]}"#;
        let p = crate::io::load(junk).unwrap();
        assert_eq!(p.session_quant, Quant::Bar);
        let s = p.session_slot(1, 2);
        assert_eq!(s.launch, LaunchMode::Trigger);
        assert_eq!(s.follow, Follow { bars: Follow::MAX_BARS, a: FollowAction::Next, b: FollowAction::Any, chance_a: 0 });
        assert_eq!(p.session_scenes[2].follow, Follow::OFF);
    }
}
