//! MIDI learn: links from a controller (CC, note, pitch bend) to anything that
//! moves in Ponzik (device parameters, mixer, sends, macros, transport).
//!
//! Pure logic only: the interface feeds incoming messages and current values,
//! this module says what the new value is. Mappings live either in the project
//! (per song) or in the settings (the controller layout), with the same JSON.

use ponzik_engine::AutoTarget;
use ponzik_engine::mixer::{MAX_TRACKS, SEND_SLOTS};

use crate::io::{load_target, target_json};
use crate::json::Json;

/// A transport button that a controller can press.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TransportAction {
    /// Play / pause.
    Play,
    Stop,
    /// Arm or disarm recording.
    Record,
    Metronome,
}

impl TransportAction {
    pub const ALL: [TransportAction; 4] = [Self::Play, Self::Stop, Self::Record, Self::Metronome];

    pub fn id(self) -> &'static str {
        match self {
            Self::Play => "play",
            Self::Stop => "stop",
            Self::Record => "record",
            Self::Metronome => "metronome",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.id() == id)
    }
}

/// What a controller drives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlTarget {
    /// Any automatable parameter: devices, plugins, macros, volumes, pans, tempo.
    Param(AutoTarget),
    /// Level of a mixer send.
    Send { track: usize, slot: usize },
    Transport(TransportAction),
    /// Launches a scene of the Session view (held: gate slots play).
    Scene(usize),
    /// Launches a slot of the Session view (held: a gate slot plays).
    Slot { col: usize, row: usize },
}

impl ControlTarget {
    /// Pressed rather than turned.
    pub fn is_button(self) -> bool {
        matches!(self, Self::Transport(_) | Self::Scene(_) | Self::Slot { .. })
    }
}

/// Where a controller message comes from. Channels are 0..=15.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CtlSource {
    Cc { channel: u8, cc: u8 },
    Note { channel: u8, key: u8 },
    PitchBend { channel: u8 },
}

/// One incoming controller message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CtlValue {
    /// A 7-bit controller value.
    Cc(u8),
    /// Note on (velocity > 0) or off.
    Note { on: bool, velocity: u8 },
    /// 14-bit pitch bend, 0..=16383, 8192 at rest.
    Bend(u16),
}

impl CtlValue {
    /// Position in [0, 1] for absolute use.
    pub fn norm(self) -> f32 {
        match self {
            Self::Cc(v) => v.min(127) as f32 / 127.0,
            Self::Note { on, velocity } => if on { velocity.max(1) as f32 / 127.0 } else { 0.0 },
            Self::Bend(v) => v.min(16383) as f32 / 16383.0,
        }
    }

    /// Button state: note held, or a controller in its upper half.
    pub fn pressed(self) -> bool {
        match self {
            Self::Cc(v) => v >= 64,
            Self::Note { on, .. } => on,
            Self::Bend(v) => v >= 12288,
        }
    }
}

/// Encodings of endless encoders (relative controllers).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    /// 1..=63 up, 127 = −1 down to 65 = −63.
    TwosComplement,
    /// 64 is rest: 65 = +1, 63 = −1.
    BinaryOffset,
    /// Bit 6 is the sign: 1 = +1, 65 = −1.
    SignMagnitude,
}

impl Encoding {
    pub fn id(self) -> &'static str {
        match self {
            Self::TwosComplement => "twos_complement",
            Self::BinaryOffset => "binary_offset",
            Self::SignMagnitude => "sign_magnitude",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        [Self::TwosComplement, Self::BinaryOffset, Self::SignMagnitude].into_iter().find(|e| e.id() == id)
    }

    /// Steps of one message (0 for a value that means "no move").
    pub fn decode(self, v: u8) -> i32 {
        let v = (v & 0x7f) as i32;
        match self {
            Self::TwosComplement => if v < 64 { v } else { v - 128 },
            Self::BinaryOffset => v - 64,
            Self::SignMagnitude => if v & 64 != 0 { -(v & 63) } else { v & 63 },
        }
    }

    /// The encoding these values were sent in, if they look like an endless
    /// encoder at all: small steps that repeat, around 0 (two's complement and
    /// sign-magnitude) or around 64 (binary offset).
    pub fn detect(values: &[u8]) -> Option<Encoding> {
        if values.is_empty() {
            return None;
        }
        let small_up = |v: u8| (1..=15).contains(&v);
        let tc_down = |v: u8| (113..=127).contains(&v);
        let sm_down = |v: u8| (65..=79).contains(&v);
        let around_64 = |v: u8| (49..=79).contains(&v) && v != 64;
        if values.iter().all(|&v| small_up(v) || tc_down(v)) {
            return Some(Encoding::TwosComplement);
        }
        if values.iter().all(|&v| small_up(v) || sm_down(v)) && values.iter().any(|&v| small_up(v)) {
            return Some(Encoding::SignMagnitude);
        }
        if values.iter().all(|&v| around_64(v)) {
            return Some(Encoding::BinaryOffset);
        }
        None
    }
}

/// How the controller's value turns into the parameter's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CtlMode {
    /// The controller's position is the value (knobs, faders, pitch bend).
    Absolute,
    /// Endless encoders: each message moves the value; `None` detects the
    /// encoding from the messages.
    Relative(Option<Encoding>),
    /// Buttons: each press flips between the two ends of the range.
    Toggle,
}

impl CtlMode {
    pub fn id(self) -> &'static str {
        match self {
            Self::Absolute => "absolute",
            Self::Relative(_) => "relative",
            Self::Toggle => "toggle",
        }
    }

    /// The learn result: what the messages seen while learning look like.
    pub fn detect(source: CtlSource, values: &[u8]) -> CtlMode {
        match source {
            CtlSource::Note { .. } => return CtlMode::Toggle,
            CtlSource::PitchBend { .. } => return CtlMode::Absolute,
            CtlSource::Cc { .. } => {}
        }
        if !values.is_empty() && values.iter().all(|&v| v == 0 || v == 127) {
            return CtlMode::Toggle;
        }
        // Knobs and faders only send changes: an encoder repeats the same step.
        let repeats = values.windows(2).filter(|w| w[0] == w[1]).count();
        if values.len() >= 3 && repeats * 2 >= values.len() - 1 {
            if let Some(e) = Encoding::detect(values) {
                return CtlMode::Relative(Some(e));
            }
        }
        CtlMode::Absolute
    }
}

/// One controller link.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MidiMapping {
    pub source: CtlSource,
    pub target: ControlTarget,
    pub mode: CtlMode,
    /// Range of the parameter in normalized units [0, 1]; `min > max` inverts.
    pub min: f32,
    pub max: f32,
    /// Soft takeover: an absolute control acts only once it meets the value.
    pub pickup: bool,
}

impl MidiMapping {
    pub fn new(source: CtlSource, target: ControlTarget, mode: CtlMode) -> Self {
        Self { source, target, mode, min: 0.0, max: 1.0, pickup: true }
    }

    /// Next value of the parameter (normalized) for this message, given its
    /// current value, or what a button does. Updates the runtime state.
    pub fn apply(&self, state: &mut CtlState, value: CtlValue, current: f32) -> Outcome {
        // Moved by something else (mouse, automation, undo): pick up again.
        if let Some(last) = state.last_out
            && (last - current).abs() > 0.004
        {
            state.picked = false;
        }
        if self.target.is_button() {
            let (was, now) = (state.down, value.pressed());
            state.down = now;
            return match (was, now) {
                (false, true) => Outcome::Trigger,
                (true, false) => Outcome::Release,
                _ => Outcome::None,
            };
        }
        let span = self.max - self.min;
        let out = match (self.mode, value) {
            (CtlMode::Toggle, v) => {
                let press = v.pressed() && !state.down;
                state.down = v.pressed();
                if !press {
                    return Outcome::None;
                }
                // To the far end from where it is.
                let mid = (self.min + self.max) * 0.5;
                let at_max = if span >= 0.0 { current > mid } else { current < mid };
                if at_max { self.min } else { self.max }
            }
            (CtlMode::Relative(enc), CtlValue::Cc(v)) => {
                state.push(v);
                let enc = enc.or_else(|| Encoding::detect(state.history())).unwrap_or(Encoding::TwosComplement);
                let steps = enc.decode(v);
                if steps == 0 || span.abs() < 1e-6 {
                    return Outcome::None;
                }
                let r = ((current - self.min) / span).clamp(0.0, 1.0) + steps as f32 / 128.0;
                self.min + r.clamp(0.0, 1.0) * span
            }
            (_, v) => {
                let target = self.min + v.norm() * span;
                if !self.pickup {
                    state.picked = true;
                }
                if !state.picked {
                    let near = (target - current).abs() <= 0.02;
                    let crossed = state.prev_in.is_some_and(|p| (p - current) * (target - current) <= 0.0);
                    state.picked = near || crossed;
                }
                state.prev_in = Some(target);
                if !state.picked {
                    return Outcome::Waiting(target);
                }
                target
            }
        };
        let out = out.clamp(0.0, 1.0);
        state.last_out = Some(out);
        state.picked = true;
        Outcome::Set(out)
    }
}

/// What a controller message does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Outcome {
    None,
    /// New normalized value.
    Set(f32),
    /// A button press.
    Trigger,
    /// The button is let go.
    Release,
    /// Pickup: the control is at this value, not yet at the parameter's.
    Waiting(f32),
}

/// Runtime state of one mapping (not saved).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CtlState {
    /// Last controller position, in parameter units (pickup crossing).
    prev_in: Option<f32>,
    /// The control has met the parameter's value.
    picked: bool,
    /// Last value set by this control.
    last_out: Option<f32>,
    /// Button held.
    down: bool,
    /// Last relative values, for the encoding detection.
    hist: [u8; 8],
    hist_len: u8,
}

impl CtlState {
    fn push(&mut self, v: u8) {
        if (self.hist_len as usize) < self.hist.len() {
            self.hist[self.hist_len as usize] = v;
            self.hist_len += 1;
        } else {
            self.hist.rotate_left(1);
            self.hist[self.hist.len() - 1] = v;
        }
    }

    fn history(&self) -> &[u8] {
        &self.hist[..self.hist_len as usize]
    }

    /// The value the parameter really took for the last [`Outcome::Set`]
    /// (rounded or snapped by it): not mistaken for a move made elsewhere.
    pub fn settle(&mut self, actual: f32) {
        if self.last_out.is_some() {
            self.last_out = Some(actual);
        }
    }

    /// The control is following the parameter (no pickup pending).
    pub fn picked(&self) -> bool {
        self.picked
    }
}

/// How long learning listens to the moved control before deciding its mode.
pub const LEARN_WINDOW: f64 = 0.6;
/// Messages enough to decide without waiting for the window.
const LEARN_VALUES: usize = 8;
/// Learning gives up after this long without any message.
pub const LEARN_TIMEOUT: f64 = 30.0;

/// The learn flow: armed on a target, the first control that moves is
/// listened to for a short while, then linked.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Learn {
    #[default]
    Idle,
    Waiting { target: ControlTarget, since: f64 },
    Listening { target: ControlTarget, source: CtlSource, values: Vec<u8>, since: f64 },
}

/// Result of a learn step.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LearnEvent {
    Linked(MidiMapping),
    TimedOut,
}

impl Learn {
    pub fn start(&mut self, target: ControlTarget, now: f64) {
        *self = Learn::Waiting { target, since: now };
    }

    pub fn cancel(&mut self) {
        *self = Learn::Idle;
    }

    pub fn target(&self) -> Option<ControlTarget> {
        match self {
            Learn::Idle => None,
            Learn::Waiting { target, .. } | Learn::Listening { target, .. } => Some(*target),
        }
    }

    pub fn is_active(&self) -> bool {
        !matches!(self, Learn::Idle)
    }

    /// A controller message. Returns the mapping when it is decided.
    /// While learning, every message is swallowed (it must not move anything).
    pub fn feed(&mut self, source: CtlSource, value: CtlValue, now: f64) -> Option<LearnEvent> {
        match self {
            Learn::Idle => None,
            Learn::Waiting { target, .. } => {
                let target = *target;
                match value {
                    // A note off or a released button does not start a link.
                    CtlValue::Note { on: false, .. } => None,
                    CtlValue::Note { .. } => self.finish(target, source, &[]),
                    CtlValue::Cc(v) => {
                        *self = Learn::Listening { target, source, values: vec![v], since: now };
                        None
                    }
                    CtlValue::Bend(_) => self.finish(target, source, &[]),
                }
            }
            Learn::Listening { target, source: s, values, since } => {
                if *s != source {
                    return None;
                }
                if let CtlValue::Cc(v) = value {
                    values.push(v);
                }
                if values.len() >= LEARN_VALUES || now - *since >= LEARN_WINDOW {
                    let (target, source, values) = (*target, *s, std::mem::take(values));
                    return self.finish(target, source, &values);
                }
                None
            }
        }
    }

    /// Time passes: the listening window closes, or waiting gives up.
    pub fn tick(&mut self, now: f64) -> Option<LearnEvent> {
        match self {
            Learn::Idle => None,
            Learn::Waiting { since, .. } => {
                if now - *since >= LEARN_TIMEOUT {
                    *self = Learn::Idle;
                    return Some(LearnEvent::TimedOut);
                }
                None
            }
            Learn::Listening { target, source, values, since } => {
                if now - *since < LEARN_WINDOW {
                    return None;
                }
                let (target, source, values) = (*target, *source, std::mem::take(values));
                self.finish(target, source, &values)
            }
        }
    }

    fn finish(&mut self, target: ControlTarget, source: CtlSource, values: &[u8]) -> Option<LearnEvent> {
        *self = Learn::Idle;
        let mode = if target.is_button() { CtlMode::Toggle } else { CtlMode::detect(source, values) };
        Some(LearnEvent::Linked(MidiMapping::new(source, target, mode)))
    }
}

/// Short label of a source: "CC 74 · 1", "C4 · 1", "Bend · 1" (channels from 1).
pub fn source_label(s: CtlSource) -> String {
    match s {
        CtlSource::Cc { channel, cc } => format!("CC {cc} · {}", channel + 1),
        CtlSource::Note { channel, key } => {
            const NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
            format!("{}{} · {}", NAMES[key as usize % 12], key as i32 / 12 - 1, channel + 1)
        }
        CtlSource::PitchBend { channel } => ponzik_i18n::tr_fmt!("Bend · {}", channel + 1),
    }
}

// --- JSON ---

fn obj(pairs: Vec<(&str, Json)>) -> Json {
    Json::Obj(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

pub fn mapping_json(m: &MidiMapping) -> Json {
    let source = match m.source {
        CtlSource::Cc { channel, cc } => obj(vec![("kind", "cc".into()), ("channel", channel.into()), ("cc", cc.into())]),
        CtlSource::Note { channel, key } => obj(vec![("kind", "note".into()), ("channel", channel.into()), ("key", key.into())]),
        CtlSource::PitchBend { channel } => obj(vec![("kind", "bend".into()), ("channel", channel.into())]),
    };
    let target = match m.target {
        ControlTarget::Param(t) => obj(vec![("kind", "param".into()), ("param", target_json(t))]),
        ControlTarget::Send { track, slot } => obj(vec![("kind", "send".into()), ("track", track.into()), ("slot", slot.into())]),
        ControlTarget::Transport(a) => obj(vec![("kind", "transport".into()), ("action", a.id().into())]),
        ControlTarget::Scene(row) => obj(vec![("kind", "scene".into()), ("row", row.into())]),
        ControlTarget::Slot { col, row } => obj(vec![("kind", "slot".into()), ("col", col.into()), ("row", row.into())]),
    };
    let mut pairs = vec![
        ("source", source),
        ("target", target),
        ("mode", m.mode.id().into()),
        ("min", m.min.into()),
        ("max", m.max.into()),
        ("pickup", m.pickup.into()),
    ];
    if let CtlMode::Relative(Some(e)) = m.mode {
        pairs.push(("encoding", e.id().into()));
    }
    obj(pairs)
}

fn small(v: Option<&Json>, limit: u32) -> Option<u8> {
    let n = v?.as_f64()?;
    (n >= 0.0 && n.fract() == 0.0 && n < limit as f64).then_some(n as u8)
}

fn index(v: Option<&Json>, limit: usize) -> Option<usize> {
    let n = v?.as_f64()?;
    (n >= 0.0 && n.fract() == 0.0 && n < limit as f64).then_some(n as usize)
}

/// A mapping read back; `None` if it is not readable. `channels`: how many
/// channels parameter targets may refer to (`usize::MAX` for the settings).
pub fn load_mapping(v: &Json, channels: usize) -> Option<MidiMapping> {
    let s = v.get("source")?;
    let channel = small(s.get("channel"), 16)?;
    let source = match s.get("kind")?.as_str()? {
        "cc" => CtlSource::Cc { channel, cc: small(s.get("cc"), 128)? },
        "note" => CtlSource::Note { channel, key: small(s.get("key"), 128)? },
        "bend" => CtlSource::PitchBend { channel },
        _ => return None,
    };
    let t = v.get("target")?;
    let target = match t.get("kind")?.as_str()? {
        "param" => ControlTarget::Param(load_target(t.get("param")?, channels)?),
        "send" => ControlTarget::Send { track: index(t.get("track"), MAX_TRACKS)?, slot: index(t.get("slot"), SEND_SLOTS)? },
        "transport" => ControlTarget::Transport(TransportAction::from_id(t.get("action")?.as_str()?)?),
        "scene" => ControlTarget::Scene(index(t.get("row"), crate::SESSION_SCENES)?),
        "slot" => ControlTarget::Slot { col: index(t.get("col"), crate::SESSION_COLUMNS)?, row: index(t.get("row"), crate::SESSION_SCENES)? },
        _ => return None,
    };
    let mode = match v.get("mode").and_then(Json::as_str) {
        Some("relative") => CtlMode::Relative(v.get("encoding").and_then(Json::as_str).and_then(Encoding::from_id)),
        Some("toggle") => CtlMode::Toggle,
        _ => CtlMode::Absolute,
    };
    let unit = |k: &str, d: f32| v.get(k).and_then(Json::as_f64).filter(|n| n.is_finite()).map_or(d, |n| (n as f32).clamp(0.0, 1.0));
    Some(MidiMapping {
        source,
        target,
        mode,
        min: unit("min", 0.0),
        max: unit("max", 1.0),
        pickup: v.get("pickup").and_then(Json::as_bool).unwrap_or(true),
    })
}

pub fn mappings_json(list: &[MidiMapping]) -> Json {
    Json::Arr(list.iter().map(mapping_json).collect())
}

pub fn load_mappings(v: Option<&Json>, channels: usize) -> Vec<MidiMapping> {
    v.and_then(Json::as_arr).unwrap_or(&[]).iter().filter_map(|m| load_mapping(m, channels)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CC: CtlSource = CtlSource::Cc { channel: 0, cc: 74 };
    const CUTOFF: ControlTarget = ControlTarget::Param(AutoTarget::InstrumentParam { channel: 1, id: 3 });

    fn set(o: Outcome) -> f32 {
        match o {
            Outcome::Set(v) => v,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn absolute_follows_the_range_and_inverts() {
        let mut m = MidiMapping::new(CC, CUTOFF, CtlMode::Absolute);
        m.pickup = false;
        let mut s = CtlState::default();
        assert_eq!(set(m.apply(&mut s, CtlValue::Cc(127), 0.5)), 1.0);
        assert_eq!(set(m.apply(&mut s, CtlValue::Cc(0), 1.0)), 0.0);
        m.min = 0.2;
        m.max = 0.6;
        assert!((set(m.apply(&mut s, CtlValue::Cc(127), 0.0)) - 0.6).abs() < 1e-6);
        let mid = set(m.apply(&mut s, CtlValue::Cc(64), 0.6));
        assert!((mid - (0.2 + 0.4 * 64.0 / 127.0)).abs() < 1e-6);
        // Inverted: the top of the control is the bottom of the range.
        (m.min, m.max) = (1.0, 0.0);
        assert_eq!(set(m.apply(&mut s, CtlValue::Cc(127), mid)), 0.0);
        // Pitch bend uses its 14 bits.
        let bend = MidiMapping { pickup: false, ..MidiMapping::new(CtlSource::PitchBend { channel: 0 }, CUTOFF, CtlMode::Absolute) };
        assert_eq!(set(bend.apply(&mut CtlState::default(), CtlValue::Bend(16383), 0.0)), 1.0);
    }

    #[test]
    fn pickup_waits_for_the_value_then_follows() {
        let m = MidiMapping::new(CC, CUTOFF, CtlMode::Absolute);
        let mut s = CtlState::default();
        // The parameter sits at 0.5; the knob starts low: no jump.
        assert!(matches!(m.apply(&mut s, CtlValue::Cc(10), 0.5), Outcome::Waiting(_)));
        assert!(matches!(m.apply(&mut s, CtlValue::Cc(40), 0.5), Outcome::Waiting(_)));
        // It crosses the value: from now on it follows.
        let v = set(m.apply(&mut s, CtlValue::Cc(70), 0.5));
        assert!((v - 70.0 / 127.0).abs() < 1e-6);
        assert!(s.picked());
        let v2 = set(m.apply(&mut s, CtlValue::Cc(20), v));
        assert!((v2 - 20.0 / 127.0).abs() < 1e-6);
        // Moved with the mouse meanwhile: pickup again.
        assert!(matches!(m.apply(&mut s, CtlValue::Cc(22), 0.9), Outcome::Waiting(_)));
        // Close enough counts as met.
        let near = set(m.apply(&mut s, CtlValue::Cc(114), 0.9));
        assert!((near - 114.0 / 127.0).abs() < 1e-6);
    }

    #[test]
    fn relative_encodings_decode_and_move_in_steps() {
        assert_eq!(Encoding::TwosComplement.decode(1), 1);
        assert_eq!(Encoding::TwosComplement.decode(127), -1);
        assert_eq!(Encoding::TwosComplement.decode(120), -8);
        assert_eq!(Encoding::BinaryOffset.decode(65), 1);
        assert_eq!(Encoding::BinaryOffset.decode(60), -4);
        assert_eq!(Encoding::SignMagnitude.decode(3), 3);
        assert_eq!(Encoding::SignMagnitude.decode(66), -2);
        for enc in [Encoding::TwosComplement, Encoding::BinaryOffset, Encoding::SignMagnitude] {
            let m = MidiMapping::new(CC, CUTOFF, CtlMode::Relative(Some(enc)));
            let up = match enc {
                Encoding::BinaryOffset => 68,
                _ => 4,
            };
            let mut s = CtlState::default();
            let v = set(m.apply(&mut s, CtlValue::Cc(up), 0.5));
            assert!((v - (0.5 + 4.0 / 128.0)).abs() < 1e-6, "{enc:?}");
            // Clamped at the ends; pickup does not apply to encoders.
            assert_eq!(set(m.apply(&mut s, CtlValue::Cc(up), 0.999)), 1.0);
        }
        // Within a range: steps are relative to the range.
        let m = MidiMapping { min: 0.5, max: 0.75, ..MidiMapping::new(CC, CUTOFF, CtlMode::Relative(Some(Encoding::TwosComplement))) };
        let v = set(m.apply(&mut CtlState::default(), CtlValue::Cc(127), 0.75));
        assert!((v - (0.75 - 0.25 / 128.0)).abs() < 1e-6);
    }

    #[test]
    fn relative_auto_detects_the_encoding() {
        assert_eq!(Encoding::detect(&[1, 1, 2, 127, 126]), Some(Encoding::TwosComplement));
        assert_eq!(Encoding::detect(&[65, 65, 63, 62]), Some(Encoding::BinaryOffset));
        assert_eq!(Encoding::detect(&[1, 2, 65, 66]), Some(Encoding::SignMagnitude));
        assert_eq!(Encoding::detect(&[30, 31, 32]), None);
        // Auto at runtime: binary offset seen after a few messages.
        let m = MidiMapping::new(CC, CUTOFF, CtlMode::Relative(None));
        let mut s = CtlState::default();
        let mut v = 0.5;
        for x in [63, 63, 63] {
            v = set(m.apply(&mut s, CtlValue::Cc(x), v));
        }
        assert!((v - (0.5 - 3.0 / 128.0)).abs() < 1e-6, "{v}");
    }

    #[test]
    fn toggle_flips_on_presses_only() {
        let m = MidiMapping::new(CtlSource::Note { channel: 9, key: 36 }, CUTOFF, CtlMode::Toggle);
        let mut s = CtlState::default();
        assert_eq!(set(m.apply(&mut s, CtlValue::Note { on: true, velocity: 100 }, 0.0)), 1.0);
        assert_eq!(m.apply(&mut s, CtlValue::Note { on: false, velocity: 0 }, 1.0), Outcome::None);
        assert_eq!(set(m.apply(&mut s, CtlValue::Note { on: true, velocity: 3 }, 1.0)), 0.0);
        // A CC button: 127 then 0.
        let cc = MidiMapping::new(CC, CUTOFF, CtlMode::Toggle);
        let mut s = CtlState::default();
        assert_eq!(set(cc.apply(&mut s, CtlValue::Cc(127), 0.2)), 1.0);
        assert_eq!(cc.apply(&mut s, CtlValue::Cc(127), 1.0), Outcome::None);
        assert_eq!(cc.apply(&mut s, CtlValue::Cc(0), 1.0), Outcome::None);
        assert_eq!(set(cc.apply(&mut s, CtlValue::Cc(127), 1.0)), 0.0);
        // Transport buttons trigger once per press.
        let play = MidiMapping::new(CC, ControlTarget::Transport(TransportAction::Play), CtlMode::Absolute);
        let mut s = CtlState::default();
        assert_eq!(play.apply(&mut s, CtlValue::Cc(127), 0.0), Outcome::Trigger);
        assert_eq!(play.apply(&mut s, CtlValue::Cc(127), 0.0), Outcome::None);
        assert_eq!(play.apply(&mut s, CtlValue::Cc(0), 0.0), Outcome::Release);
        assert_eq!(play.apply(&mut s, CtlValue::Cc(100), 0.0), Outcome::Trigger);
        // Session pads: press and release (gate slots).
        let pad = MidiMapping::new(CtlSource::Note { channel: 9, key: 36 }, ControlTarget::Slot { col: 2, row: 5 }, CtlMode::Toggle);
        let mut s = CtlState::default();
        assert_eq!(pad.apply(&mut s, CtlValue::Note { on: true, velocity: 90 }, 0.0), Outcome::Trigger);
        assert_eq!(pad.apply(&mut s, CtlValue::Note { on: false, velocity: 0 }, 0.0), Outcome::Release);
        assert_eq!(pad.apply(&mut s, CtlValue::Note { on: false, velocity: 0 }, 0.0), Outcome::None);
        for target in [ControlTarget::Scene(7), ControlTarget::Slot { col: 2, row: 5 }] {
            let m = MidiMapping::new(CtlSource::Note { channel: 0, key: 40 }, target, CtlMode::Toggle);
            assert_eq!(load_mapping(&mapping_json(&m), 4), Some(m));
        }
        let bad = mapping_json(&MidiMapping::new(CC, ControlTarget::Scene(99), CtlMode::Toggle));
        assert_eq!(load_mapping(&bad, 4), None);
    }

    fn linked(e: Option<LearnEvent>) -> MidiMapping {
        match e {
            Some(LearnEvent::Linked(m)) => m,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn learn_listens_then_links_with_the_detected_mode() {
        let mut l = Learn::default();
        assert_eq!(l.feed(CC, CtlValue::Cc(3), 0.0), None);
        l.start(CUTOFF, 1.0);
        assert_eq!(l.target(), Some(CUTOFF));
        // A note off does not start anything; other sources are ignored once listening.
        assert_eq!(l.feed(CtlSource::Note { channel: 0, key: 60 }, CtlValue::Note { on: false, velocity: 0 }, 1.1), None);
        assert_eq!(l.feed(CC, CtlValue::Cc(40), 1.2), None);
        assert!(matches!(l, Learn::Listening { .. }));
        assert_eq!(l.feed(CtlSource::Cc { channel: 0, cc: 1 }, CtlValue::Cc(9), 1.25), None);
        assert_eq!(l.feed(CC, CtlValue::Cc(41), 1.3), None);
        assert_eq!(l.tick(1.5), None);
        let m = linked(l.tick(1.9));
        assert_eq!((m.source, m.target, m.mode), (CC, CUTOFF, CtlMode::Absolute));
        assert!(!l.is_active());

        // An encoder: repeated small steps, decided after enough messages.
        l.start(CUTOFF, 0.0);
        let mut out = None;
        for (i, v) in [1u8, 1, 1, 2, 1, 1, 1, 1].into_iter().enumerate() {
            out = l.feed(CC, CtlValue::Cc(v), 0.01 * i as f64).or(out);
        }
        assert_eq!(linked(out).mode, CtlMode::Relative(Some(Encoding::TwosComplement)));

        // A pad: a note is a button.
        l.start(CUTOFF, 0.0);
        let pad = CtlSource::Note { channel: 9, key: 36 };
        assert_eq!(linked(l.feed(pad, CtlValue::Note { on: true, velocity: 90 }, 0.1)).mode, CtlMode::Toggle);
        // A CC button sends 127 then 0.
        l.start(CUTOFF, 0.0);
        l.feed(CC, CtlValue::Cc(127), 0.0);
        l.feed(CC, CtlValue::Cc(0), 0.1);
        assert_eq!(linked(l.tick(1.0)).mode, CtlMode::Toggle);
        // Nothing moves: learning gives up.
        l.start(CUTOFF, 0.0);
        assert_eq!(l.tick(LEARN_TIMEOUT + 1.0), Some(LearnEvent::TimedOut));
        l.start(CUTOFF, 0.0);
        l.cancel();
        assert_eq!(l, Learn::Idle);
    }

    #[test]
    fn mappings_round_trip_in_json() {
        let list = vec![
            MidiMapping { min: 0.25, max: 0.5, pickup: false, ..MidiMapping::new(CC, CUTOFF, CtlMode::Absolute) },
            MidiMapping::new(CtlSource::Note { channel: 9, key: 36 }, ControlTarget::Transport(TransportAction::Record), CtlMode::Toggle),
            MidiMapping::new(CtlSource::PitchBend { channel: 2 }, ControlTarget::Send { track: 3, slot: 1 }, CtlMode::Absolute),
            MidiMapping::new(CC, ControlTarget::Param(AutoTarget::TrackGain(0)), CtlMode::Relative(Some(Encoding::SignMagnitude))),
            MidiMapping::new(CC, ControlTarget::Param(AutoTarget::Tempo), CtlMode::Relative(None)),
        ];
        let text = mappings_json(&list).pretty();
        let back = load_mappings(Some(&crate::json::parse(&text).unwrap()), 4);
        assert_eq!(back, list);
        // A parameter of a missing channel, junk: dropped.
        let junk = r#"[{"source": {"kind": "cc", "channel": 0, "cc": 7}, "target": {"kind": "param", "param": {"kind": "channel_gain", "channel": 9}}},
            {"source": {"kind": "cc", "channel": 16, "cc": 7}, "target": {"kind": "transport", "action": "play"}}, 3]"#;
        assert!(load_mappings(Some(&crate::json::parse(junk).unwrap()), 4).is_empty());
        assert_eq!(source_label(CtlSource::Note { channel: 9, key: 60 }), "C4 · 10");
    }

    #[test]
    fn projects_keep_their_mappings() {
        let mut p = crate::Project::empty();
        p.midi_map.push(MidiMapping::new(CC, ControlTarget::Param(AutoTarget::TrackPan(2)), CtlMode::Absolute));
        let back = crate::load(&crate::save(&p)).unwrap();
        assert_eq!(back.midi_map, p.midi_map);
    }
}
