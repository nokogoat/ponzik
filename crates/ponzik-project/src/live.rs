//! Live performance settings saved with the project: the playlist's
//! performance mode (FL Studio) and the crossfader (Ableton Live).
//!
//! Performance mode: the playlist is cut in two by the performance marker.
//! Left of it, the arrangement plays as a song; from it on, the
//! performance zone is a bank of clips launched live, one at a time per
//! lane, on a launch quantization (see [`crate::perform`]). While the mode is
//! on, the zone's clips are left out of the song.
//!
//! Crossfader: each mixer track listens to side A, side B or neither
//! ([`crate::TrackDef::xfade`]); the position and curve are here, the gains
//! in [`ponzik_engine::xfader`].

use ponzik_engine::xfader::Curve;
use ponzik_i18n::tr_noop;

use crate::json::Json;
use crate::session::Quant;

/// How a performance clip reacts to its pad (mouse, key or MIDI note).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PerfLaunch {
    /// A press starts the clip; pressing it again while it plays changes nothing.
    #[default]
    Trigger,
    /// The clip plays while the pad is held.
    Gate,
    /// A press starts the clip, the next one stops it.
    Toggle,
    /// Each press starts the clip over, on the next boundary.
    Retrigger,
}

impl PerfLaunch {
    pub const ALL: [PerfLaunch; 4] = [Self::Trigger, Self::Gate, Self::Toggle, Self::Retrigger];

    pub fn id(self) -> &'static str {
        match self {
            Self::Trigger => "trigger",
            Self::Gate => "gate",
            Self::Toggle => "toggle",
            Self::Retrigger => "retrigger",
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
            Self::Retrigger => tr_noop("Retrigger"),
        }
    }
}

/// Launch quantizations offered by performance mode.
pub const PERF_QUANTS: [Quant; 4] = [Quant::None, Quant::Beat, Quant::Bar, Quant::Bars2];

/// The playlist's performance mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Perf {
    pub on: bool,
    /// Tick of the performance marker: clips that start there or later are
    /// launched live.
    pub zone: u32,
    pub quant: Quant,
    pub launch: PerfLaunch,
}

impl Default for Perf {
    fn default() -> Self {
        Self { on: false, zone: 0, quant: Quant::Bar, launch: PerfLaunch::Trigger }
    }
}

impl Perf {
    /// True if a clip starting at `start` is a performance clip.
    pub fn in_zone(&self, start: u32) -> bool {
        self.on && start >= self.zone
    }
}

/// The master crossfader.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Crossfader {
    /// 0: side A only, 1: side B only.
    pub pos: f32,
    pub curve: Curve,
    /// Shown in the mixer (A/B and cue buttons on the strips).
    pub shown: bool,
    /// Blend of the cue bus into what the device plays while a track is
    /// cued: 0 the master only, 1 the cue only.
    pub cue_mix: f32,
}

impl Default for Crossfader {
    fn default() -> Self {
        Self { pos: 0.5, curve: Curve::Power, shown: false, cue_mix: 1.0 }
    }
}

impl Crossfader {
    /// Gain table for the engine ([`ponzik_engine::Command::SetCrossfader`]).
    pub fn table(&self) -> [f32; 3] {
        ponzik_engine::xfader::table(self.pos, self.curve)
    }
}

/// A performance clip playing (or about to): a copy of the clip as it was
/// launched, from `start` to `end` (ticks, `u32::MAX` until replaced).
#[derive(Clone, Debug, PartialEq)]
pub struct PerfLive {
    pub lane: usize,
    pub clip: crate::perform::PerfClip,
    pub start: u32,
    pub end: u32,
}

/// Live performance state of a project.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Live {
    pub perf: Perf,
    pub xfader: Crossfader,
    /// Performance clips the engine plays (not saved).
    pub playing: Vec<PerfLive>,
}

fn obj(pairs: Vec<(&str, Json)>) -> Json {
    Json::Obj(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

/// Fields for the project file; nothing when everything is at its default.
pub(crate) fn live_json(l: &Live) -> Vec<(&'static str, Json)> {
    let mut out = Vec::new();
    if l.perf != Perf::default() {
        let p = &l.perf;
        out.push((
            "performance",
            obj(vec![
                ("on", p.on.into()),
                ("zone", p.zone.into()),
                ("quant", p.quant.id().into()),
                ("launch", p.launch.id().into()),
            ]),
        ));
    }
    if l.xfader != Crossfader::default() {
        let x = &l.xfader;
        out.push((
            "crossfader",
            obj(vec![
                ("pos", x.pos.into()),
                ("curve", x.curve.id().into()),
                ("shown", x.shown.into()),
                ("cue_mix", x.cue_mix.into()),
            ]),
        ));
    }
    out
}

/// Reads what [`live_json`] wrote; anything missing or broken stays at its default.
pub(crate) fn load_live(root: &Json) -> Live {
    let mut l = Live::default();
    let num = |v: &Json, k: &str| v.get(k).and_then(Json::as_f64).filter(|n| n.is_finite());
    if let Some(v) = root.get("performance") {
        let p = &mut l.perf;
        p.on = v.get("on").and_then(Json::as_bool).unwrap_or(false);
        p.zone = num(v, "zone").map_or(0, |n| n.clamp(0.0, (u32::MAX / 2) as f64) as u32);
        p.quant = v.get("quant").and_then(Json::as_str).and_then(Quant::from_id).filter(|q| PERF_QUANTS.contains(q)).unwrap_or(Quant::Bar);
        p.launch = v.get("launch").and_then(Json::as_str).and_then(PerfLaunch::from_id).unwrap_or_default();
    }
    if let Some(v) = root.get("crossfader") {
        let x = &mut l.xfader;
        x.pos = num(v, "pos").map_or(0.5, |n| n.clamp(0.0, 1.0) as f32);
        x.curve = v.get("curve").and_then(Json::as_str).and_then(Curve::from_id).unwrap_or_default();
        x.shown = v.get("shown").and_then(Json::as_bool).unwrap_or(false);
        x.cue_mix = num(v, "cue_mix").map_or(1.0, |n| n.clamp(0.0, 1.0) as f32);
    }
    l
}

#[cfg(test)]
mod tests {
    use super::*;
    use ponzik_engine::xfader::Side;

    #[test]
    fn settings_roundtrip_and_tolerate_junk() {
        let mut p = crate::Project::empty();
        assert!(!crate::io::save(&p).contains("\"performance\""), "defaults are not written");
        p.live.perf = Perf { on: true, zone: 3072, quant: Quant::Bars2, launch: PerfLaunch::Retrigger };
        p.live.xfader = Crossfader { pos: 0.25, curve: Curve::Cut, shown: true, cue_mix: 0.4 };
        p.tracks[3].xfade = Side::B;
        p.tracks[4].xfade = Side::A;
        let back = crate::io::load(&crate::io::save(&p)).unwrap();
        assert_eq!(back.live.perf, p.live.perf);
        assert_eq!(back.live.xfader, p.live.xfader);
        assert_eq!(back.tracks[3].xfade, Side::B);
        assert_eq!(back.tracks[4].xfade, Side::A);
        assert_eq!(back.tracks[5].xfade, Side::None);

        let junk = r#"{"format": "ponzik-project",
            "performance": {"on": "yes", "zone": -40, "quant": "4", "launch": "wild"},
            "crossfader": {"pos": 9, "curve": "zigzag", "cue_mix": "loud"},
            "tracks": [{}, {"xfade": "c"}, {"xfade": "a"}]}"#;
        let p = crate::io::load(junk).unwrap();
        assert_eq!(p.live.perf, Perf::default());
        assert_eq!(p.live.xfader, Crossfader { pos: 1.0, ..Crossfader::default() });
        assert_eq!(p.tracks[1].xfade, Side::None);
        assert_eq!(p.tracks[2].xfade, Side::A);
    }

    #[test]
    fn the_zone_holds_the_clips_from_the_marker_on() {
        let perf = Perf { on: true, zone: 1536, ..Perf::default() };
        assert!(!perf.in_zone(1535));
        assert!(perf.in_zone(1536));
        assert!(!Perf { on: false, ..perf }.in_zone(4000), "mode off: no zone");
    }
}
