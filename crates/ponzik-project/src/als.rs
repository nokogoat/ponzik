//! Ableton Live sets (`.als`): export an interchange copy, import one.
//!
//! A Live set is gzipped XML (see [`crate::gzip`]); every value sits in an
//! attribute, mostly `Value`. Positions are in beats.
//!
//! Export writes a Live 11 set: the tempo and time signature, one MIDI track
//! per channel (name, colour, volume, pan) with its notes as arrangement clips
//! (each playlist clip of a pattern becomes one clip on every channel that
//! plays in it, loops unrolled), and the markers as locators. The tracks have
//! no instrument: Ponzik's instruments, effects, audio clips, automation and
//! mixer routing stay behind.
//!
//! Import reads MIDI tracks (name, colour, volume, pan, the instrument's kind
//! guessed from its device), their arrangement and Session clips with notes,
//! audio clips (file, place and length), the tempo, the time signature and the
//! locators.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use ponzik_engine::{Note, PPQ, PlayMode, TICKS_PER_BAR};
use ponzik_i18n::{tr, tr_fmt};

use crate::{AudioClipDef, ChannelDef, ClipDef, ImportReport, InstrumentDef, Marker, PatternDef, Project, TimeSig};

/// Live's clip and track colours (the first two rows of its palette).
const PALETTE: [u32; 28] = [
    0xFF94A6, 0xFFA529, 0xCC9927, 0xF7F47C, 0xBFFB00, 0x1AFF2F, 0x25FFA8, 0x5CFFE8, 0x8BC5FF, 0x5480E4, 0x92A7FF,
    0xD86CE4, 0xE553A0, 0xFFFFFF, 0xFF3636, 0xF66C03, 0x99724B, 0xFFF034, 0x87FF67, 0x3DC300, 0x00BFAF, 0x19E9FF,
    0x10A4EE, 0x007DC0, 0x886CE4, 0xB677C6, 0xFF39D4, 0xD0D0D0,
];
/// Live's green, for a channel without a colour of its own.
const DEFAULT_COLOR: usize = 19;
/// Clips and notes beyond this tick are dropped (garbage protection).
const MAX_TICK: u32 = TICKS_PER_BAR * 4096;
/// A Live set larger than this once unpacked is refused.
const MAX_XML: usize = 512 << 20;

fn nearest_color(rgb: u32) -> usize {
    if rgb == 0 {
        return DEFAULT_COLOR;
    }
    let ch = |c: u32, s: u32| ((c >> s) & 0xff) as i32;
    PALETTE
        .iter()
        .enumerate()
        .min_by_key(|&(_, &p)| (0..3).map(|k| (ch(p, k * 8) - ch(rgb, k * 8)).pow(2)).sum::<i32>())
        .map_or(DEFAULT_COLOR, |(i, _)| i)
}

fn beats(ticks: u32) -> String {
    let b = ticks as f64 / PPQ as f64;
    let s = format!("{b:.6}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn ticks(beats: f64) -> u32 {
    if beats.is_finite() && beats > 0.0 { (beats * PPQ as f64).round().min(MAX_TICK as f64) as u32 } else { 0 }
}

// --- Writing ---

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "&#{};", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

/// An XML document being written, tab-indented like Live's.
struct Xml {
    out: String,
    stack: Vec<&'static str>,
    /// Next id of an automation or modulation target (`NextPointeeId`).
    pointee: u32,
}

impl Xml {
    fn indent(&mut self) {
        for _ in 0..self.stack.len() {
            self.out.push('\t');
        }
    }

    fn attrs(&mut self, attrs: &[(&str, &str)]) {
        for (k, v) in attrs {
            let _ = write!(self.out, " {k}=\"{}\"", escape(v));
        }
    }

    fn open(&mut self, tag: &'static str, attrs: &[(&str, &str)]) {
        self.indent();
        self.out.push('<');
        self.out.push_str(tag);
        self.attrs(attrs);
        self.out.push_str(">\n");
        self.stack.push(tag);
    }

    fn close(&mut self) {
        let tag = self.stack.pop().expect("an open element");
        self.indent();
        let _ = writeln!(self.out, "</{tag}>");
    }

    fn leaf(&mut self, tag: &str, attrs: &[(&str, &str)]) {
        self.indent();
        self.out.push('<');
        self.out.push_str(tag);
        self.attrs(attrs);
        self.out.push_str(" />\n");
    }

    fn v(&mut self, tag: &str, value: impl std::fmt::Display) {
        self.leaf(tag, &[("Value", &value.to_string())]);
    }

    fn empty(&mut self, tag: &str) {
        self.leaf(tag, &[]);
    }

    fn next_id(&mut self) -> String {
        self.pointee += 1;
        self.pointee.to_string()
    }

    fn target(&mut self, tag: &'static str) {
        let id = self.next_id();
        self.open(tag, &[("Id", &id)]);
        self.v("LockEnvelope", 0);
        self.close();
    }

    /// An automatable parameter: its value, its range, its targets.
    fn param(&mut self, tag: &'static str, value: impl std::fmt::Display, range: Option<(&str, &str)>, modulated: bool) {
        self.open(tag, &[]);
        self.v("LomId", 0);
        self.v("Manual", value);
        if let Some((min, max)) = range {
            self.open("MidiControllerRange", &[]);
            self.v("Min", min);
            self.v("Max", max);
            self.close();
        }
        self.target("AutomationTarget");
        if modulated {
            self.target("ModulationTarget");
        }
        self.close();
    }

    fn routing(&mut self, tag: &'static str, target: &str, upper: &str, lower: &str) {
        self.open(tag, &[]);
        self.v("Target", target);
        self.v("UpperDisplayString", upper);
        self.v("LowerDisplayString", lower);
        self.open("MpeSettings", &[]);
        self.v("ZoneType", 0);
        self.v("FirstNoteChannel", 1);
        self.v("LastNoteChannel", 15);
        self.close();
        self.close();
    }

    /// The device-like header Live writes before a mixer or a sequencer.
    fn device_head(&mut self) {
        self.v("LomId", 0);
        self.v("LomIdView", 0);
        self.v("IsExpanded", "true");
        self.param("On", "true", None, false);
        self.v("ModulationSourceCount", 0);
        self.leaf("ParametersListWrapper", &[("LomId", "0")]);
        let id = self.next_id();
        self.leaf("Pointee", &[("Id", &id)]);
        self.v("LastSelectedTimeableIndex", 0);
        self.v("LastSelectedClipEnvelopeIndex", 0);
        self.open("LastPresetRef", &[]);
        self.empty("Value");
        self.close();
        self.open("LockedScripts", &[]);
        self.close();
        self.v("IsFolded", "false");
        self.v("ShouldShowPresetName", "false");
        self.v("UserName", "");
        self.v("Annotation", "");
        self.open("SourceContext", &[]);
        self.empty("Value");
        self.close();
    }

    fn mixer(&mut self, volume: f32, pan: f32, master: Option<(f32, u32)>) {
        self.open("Mixer", &[]);
        self.device_head();
        self.open("Sends", &[]);
        self.close();
        self.param("Speaker", "true", None, false);
        self.v("SoloSink", "false");
        self.v("PanMode", 0);
        self.param("Pan", pan.clamp(-1.0, 1.0), Some(("-1", "1")), true);
        self.param("SplitStereoPanL", -1, Some(("-1", "1")), true);
        self.param("SplitStereoPanR", 1, Some(("-1", "1")), true);
        self.param("Volume", volume.clamp(0.0003162278, 1.99526), Some(("0.0003162277571", "1.99526238")), true);
        self.v("ViewStateSesstionTrackWidth", 93);
        self.param("CrossFadeState", 1, Some(("0", "2")), false);
        self.leaf("SendsListWrapper", &[("LomId", "0")]);
        if let Some((bpm, sig)) = master {
            self.param("Tempo", bpm.clamp(20.0, 999.0), Some(("60", "200")), true);
            self.param("TimeSignature", sig, None, false);
            self.param("GlobalGrooveAmount", 100, Some(("0", "131.25")), true);
            self.param("CrossFade", 0, Some(("-1", "1")), true);
            self.v("TempoAutomationViewBottom", 60);
            self.v("TempoAutomationViewTop", 200);
        }
        self.close();
    }

    fn automation_lanes(&mut self) {
        self.open("AutomationLanes", &[]);
        self.open("AutomationLanes", &[]);
        self.open("AutomationLane", &[("Id", "0")]);
        self.v("SelectedDevice", 0);
        self.v("SelectedEnvelope", 0);
        self.v("IsContentSelectedInDocument", "false");
        self.v("LaneHeight", 68);
        self.close();
        self.close();
        self.v("AreAdditionalAutomationLanesFolded", "false");
        self.close();
        self.open("ClipEnvelopeChooserViewState", &[]);
        self.v("SelectedDevice", 0);
        self.v("SelectedEnvelope", 0);
        self.v("PreferModulationVisible", "false");
        self.close();
    }

    fn track_head(&mut self, name: &str, color: usize) {
        self.v("LomId", 0);
        self.v("LomIdView", 0);
        self.v("IsContentSelectedInDocument", "false");
        self.v("PreferredContentViewMode", 0);
        self.open("TrackDelay", &[]);
        self.v("Value", 0);
        self.v("IsValueSampleBased", "false");
        self.close();
        self.open("Name", &[]);
        self.v("EffectiveName", name);
        self.v("UserName", name);
        self.v("Annotation", "");
        self.v("MemorizedFirstClipName", "");
        self.close();
        self.v("Color", color);
        self.open("AutomationEnvelopes", &[]);
        self.open("Envelopes", &[]);
        self.close();
        self.close();
        self.v("TrackGroupId", -1);
        self.v("TrackUnfolded", "true");
        self.leaf("DevicesListWrapper", &[("LomId", "0")]);
        self.leaf("ClipSlotsListWrapper", &[("LomId", "0")]);
        self.v("ViewData", "{}");
        self.open("TakeLanes", &[]);
        self.open("TakeLanes", &[]);
        self.close();
        self.v("AreTakeLanesFolded", "true");
        self.close();
        self.v("LinkedTrackGroupId", -1);
    }
}

/// One arrangement clip: where it starts, how long it is, its notes from the
/// clip's start.
struct LiveClip {
    start: u32,
    len: u32,
    name: String,
    color: usize,
    notes: Vec<Note>,
}

/// The notes channel `ch` plays in playlist clip `clip`, its pattern looped
/// over the clip's length from its offset.
pub(crate) fn clip_notes(p: &Project, clip: &ClipDef, ch: usize) -> Vec<Note> {
    let Some(pat) = p.patterns.get(clip.pattern) else { return Vec::new() };
    let Some(notes) = pat.notes.get(ch).filter(|n| !n.is_empty()) else { return Vec::new() };
    let len = pat.len.max(1);
    let mut out = Vec::new();
    // Passes of the pattern that overlap the clip.
    let first = clip.offset / len;
    let last = (clip.offset + clip.len).div_ceil(len);
    for pass in first..last {
        for n in notes.iter().filter(|n| !n.slide) {
            let at = pass * len + n.start;
            if at < clip.offset || at >= clip.offset + clip.len {
                continue;
            }
            let start = at - clip.offset;
            out.push(Note { start, len: n.len.min(clip.len - start).max(1), ..*n });
        }
    }
    out.sort_by_key(|n| (n.start, n.key));
    out
}

fn midi_clip(x: &mut Xml, id: usize, c: &LiveClip) {
    let (start, end) = (beats(c.start), beats(c.start + c.len));
    let len = beats(c.len);
    x.open("MidiClip", &[("Id", &id.to_string()), ("Time", &start)]);
    x.v("LomId", 0);
    x.v("LomIdView", 0);
    x.v("CurrentStart", &start);
    x.v("CurrentEnd", &end);
    x.open("Loop", &[]);
    x.v("LoopStart", 0);
    x.v("LoopEnd", &len);
    x.v("StartRelative", 0);
    x.v("LoopOn", "false");
    x.v("OutMarker", &len);
    x.v("HiddenLoopStart", 0);
    x.v("HiddenLoopEnd", &len);
    x.close();
    x.v("Name", &c.name);
    x.v("Annotation", "");
    x.v("Color", c.color);
    x.v("LaunchMode", 0);
    x.v("LaunchQuantisation", 0);
    x.open("TimeSignature", &[]);
    x.open("TimeSignatures", &[]);
    x.open("RemoteableTimeSignature", &[("Id", "0")]);
    x.v("Numerator", 4);
    x.v("Denominator", 4);
    x.v("Time", 0);
    x.close();
    x.close();
    x.close();
    x.open("Envelopes", &[]);
    x.open("Envelopes", &[]);
    x.close();
    x.close();
    x.open("ScrollerTimePreserver", &[]);
    x.v("LeftTime", 0);
    x.v("RightTime", &len);
    x.close();
    x.open("TimeSelection", &[]);
    x.v("AnchorTime", 0);
    x.v("OtherTime", 0);
    x.close();
    x.v("Legato", "false");
    x.v("Ram", "false");
    x.open("GrooveSettings", &[]);
    x.v("GrooveId", -1);
    x.close();
    x.v("Disabled", "false");
    x.v("VelocityAmount", 0);
    x.open("FollowAction", &[]);
    x.v("FollowTime", 4);
    x.v("IsLinked", "true");
    x.v("LoopIterations", 1);
    x.v("FollowActionA", 4);
    x.v("FollowActionB", 0);
    x.v("FollowChanceA", 100);
    x.v("FollowChanceB", 0);
    x.v("JumpIndexA", 1);
    x.v("JumpIndexB", 1);
    x.v("FollowActionEnabled", "false");
    x.close();
    x.open("Grid", &[]);
    x.v("FixedNumerator", 1);
    x.v("FixedDenominator", 16);
    x.v("GridIntervalPixel", 20);
    x.v("Ntoles", 2);
    x.v("SnapToGrid", "true");
    x.v("Fixed", "false");
    x.close();
    x.v("FreezeStart", 0);
    x.v("FreezeEnd", 0);
    x.v("IsWarped", "true");
    x.v("TakeId", 1);
    x.open("Notes", &[]);
    x.open("KeyTracks", &[]);
    let mut keys: BTreeMap<u8, Vec<&Note>> = BTreeMap::new();
    for n in &c.notes {
        keys.entry(n.key.min(127)).or_default().push(n);
    }
    let mut note_id = 1;
    for (k, (key, notes)) in keys.iter().enumerate() {
        x.open("KeyTrack", &[("Id", &k.to_string())]);
        x.open("Notes", &[]);
        for n in notes {
            let vel = format!("{}", (n.vel.clamp(0.0, 1.0) * 127.0).round().max(1.0));
            let prob = format!("{}", n.prob.min(100) as f32 / 100.0);
            x.leaf(
                "MidiNoteEvent",
                &[
                    ("Time", &beats(n.start)),
                    ("Duration", &beats(n.len)),
                    ("Velocity", &vel),
                    ("VelocityDeviation", "0"),
                    ("OffVelocity", "64"),
                    ("Probability", &prob),
                    ("IsEnabled", "true"),
                    ("NoteId", &note_id.to_string()),
                ],
            );
            note_id += 1;
        }
        x.close();
        x.v("MidiKey", key);
        x.close();
    }
    x.close();
    x.open("PerNoteEventStore", &[]);
    x.open("EventLists", &[]);
    x.close();
    x.close();
    x.open("NoteIdGenerator", &[]);
    x.v("NextId", note_id);
    x.close();
    x.close();
    x.v("BankSelectCoarse", -1);
    x.v("BankSelectFine", -1);
    x.v("ProgramChange", -1);
    x.v("NoteEditorFoldInZoom", -1);
    x.v("NoteEditorFoldInScroll", 0);
    x.v("NoteEditorFoldOutZoom", 2848);
    x.v("NoteEditorFoldOutScroll", -1073);
    x.v("NoteEditorFoldScaleZoom", -1);
    x.v("NoteEditorFoldScaleScroll", 0);
    x.open("ScaleInformation", &[]);
    x.v("RootNote", 0);
    x.v("Name", "Major");
    x.close();
    x.v("IsInKey", "false");
    x.v("NoteSpellingPreference", 0);
    x.v("PreferFlatRootNote", "false");
    x.v("ExpressionGrid", "");
    x.close();
}

fn clip_slots(x: &mut Xml, scenes: usize) {
    x.open("ClipSlotList", &[]);
    for s in 0..scenes {
        x.open("ClipSlot", &[("Id", &s.to_string())]);
        x.v("LomId", 0);
        x.open("ClipSlot", &[]);
        x.empty("Value");
        x.close();
        x.v("HasStop", "true");
        x.v("NeedRefreeze", "true");
        x.close();
    }
    x.close();
}

/// The sequencers of a MIDI track: arrangement clips, empty Session slots,
/// the MIDI controller targets.
/// `head`: name, colour index, volume and pan.
fn midi_track(x: &mut Xml, id: usize, head: (&str, usize, f32, f32), clips: &[LiveClip], scenes: usize) {
    let (name, color, gain, pan) = head;
    x.open("MidiTrack", &[("Id", &id.to_string())]);
    x.track_head(name, color);
    x.v("SavedPlayingSlot", -1);
    x.v("SavedPlayingOffset", 0);
    x.v("Freeze", "false");
    x.v("VelocityDetail", 0);
    x.v("NeedArrangerRefreeze", "true");
    x.v("PostProcessFreezeClips", 0);
    x.open("DeviceChain", &[]);
    x.automation_lanes();
    x.routing("AudioInputRouting", "AudioIn/External/S0", "Ext. In", "1/2");
    x.routing("MidiInputRouting", "MidiIn/External.All/-1", "Ext: All Ins", "");
    x.routing("AudioOutputRouting", "AudioOut/Master", "Master", "");
    x.routing("MidiOutputRouting", "MidiOut/None", "None", "");
    x.mixer(gain, pan, None);
    x.open("MainSequencer", &[]);
    x.device_head();
    clip_slots(x, scenes);
    x.v("MonitoringEnum", 1);
    x.open("ClipTimeable", &[]);
    x.open("ArrangerAutomation", &[]);
    x.open("Events", &[]);
    for (k, c) in clips.iter().enumerate() {
        midi_clip(x, k, c);
    }
    x.close();
    x.open("AutomationTransformViewState", &[]);
    x.v("IsTransformPending", "false");
    x.open("TimeAndValueTransforms", &[]);
    x.close();
    x.close();
    x.close();
    x.close();
    x.open("Recorder", &[]);
    x.v("IsArmed", "false");
    x.v("TakeCounter", 1);
    x.close();
    x.open("MidiControllers", &[]);
    for c in 0..131 {
        let id = x.next_id();
        x.indent();
        let _ = writeln!(x.out, "<ControllerTargets.{c} Id=\"{id}\">");
        x.indent();
        let _ = writeln!(x.out, "\t<LockEnvelope Value=\"0\" />");
        x.indent();
        let _ = writeln!(x.out, "</ControllerTargets.{c}>");
    }
    x.close();
    x.close();
    x.open("FreezeSequencer", &[]);
    x.device_head();
    clip_slots(x, scenes);
    x.v("MonitoringEnum", 1);
    x.open("Sample", &[]);
    x.open("ArrangerAutomation", &[]);
    x.open("Events", &[]);
    x.close();
    x.open("AutomationTransformViewState", &[]);
    x.v("IsTransformPending", "false");
    x.open("TimeAndValueTransforms", &[]);
    x.close();
    x.close();
    x.close();
    x.close();
    x.target("VolumeModulationTarget");
    x.target("TranspositionModulationTarget");
    x.target("GrainSizeModulationTarget");
    x.target("FluxModulationTarget");
    x.target("SampleOffsetModulationTarget");
    x.v("PitchViewScrollPosition", -1073741824);
    x.v("SampleOffsetModulationScrollPosition", -1073741824);
    x.open("Recorder", &[]);
    x.v("IsArmed", "false");
    x.v("TakeCounter", 1);
    x.close();
    x.close();
    x.open("DeviceChain", &[]);
    x.open("Devices", &[]);
    x.close();
    x.open("SignalModulations", &[]);
    x.close();
    x.close();
    x.close();
    x.v("ReWireSlaveMidiTargetId", 0);
    x.v("PitchbendRange", 96);
    x.close();
}

/// The master track (tempo, time signature) or the cue track.
fn main_track(x: &mut Xml, tag: &'static str, bpm: f32, sig: u32) {
    x.open(tag, &[]);
    x.track_head(if tag == "MasterTrack" { "Master" } else { "" }, 13);
    x.open("DeviceChain", &[]);
    x.automation_lanes();
    x.routing("AudioInputRouting", "AudioIn/External/S0", "Ext. In", "1/2");
    x.routing("MidiInputRouting", "MidiIn/External.All/-1", "Ext: All Ins", "");
    x.routing("AudioOutputRouting", "AudioOut/External/S0", "Ext. Out", "1/2");
    x.routing("MidiOutputRouting", "MidiOut/None", "None", "");
    x.mixer(1.0, 0.0, (tag == "MasterTrack").then_some((bpm, sig)));
    x.open("FreezeSequencer", &[]);
    x.device_head();
    clip_slots(x, 0);
    x.v("MonitoringEnum", 1);
    x.close();
    x.open("DeviceChain", &[]);
    x.open("Devices", &[]);
    x.close();
    x.open("SignalModulations", &[]);
    x.close();
    x.close();
    x.close();
    x.close();
}

/// Live's encoding of a time signature: numerator − 1 + 99 × log2(denominator).
fn signature_value(num: u8, den: u8) -> u32 {
    let num = num.clamp(1, 99) as u32;
    let den = den.max(1).ilog2().min(4);
    (num - 1) + 99 * den
}

fn signature_from(value: u32) -> (u8, u8) {
    let num = (value % 99 + 1) as u8;
    let den = 1u8 << (value / 99).min(4);
    (num, den)
}

/// The XML of a Live set for `project` (see the module documentation).
pub fn export_als_xml(project: &Project) -> String {
    let p = project;
    let mut x = Xml { out: String::new(), stack: Vec::new(), pointee: 1000 };
    x.out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    x.open(
        "Ableton",
        &[
            ("MajorVersion", "5"),
            ("MinorVersion", "11.0_433"),
            ("SchemaChangeCount", "3"),
            ("Creator", "Ableton Live 11.0"),
            ("Revision", ""),
        ],
    );
    x.open("LiveSet", &[]);
    // Rewritten at the end, once every id is known.
    x.v("NextPointeeId", "@NEXT@");
    x.v("OverwriteProtectionNumber", 2816);
    x.v("LomId", 0);
    x.v("LomIdView", 0);

    // Clips per channel: each playlist clip that plays notes on the channel.
    let scenes = 8;
    let mut clips: Vec<Vec<LiveClip>> = (0..p.channels.len()).map(|_| Vec::new()).collect();
    let mut placed: Vec<&ClipDef> = p.clips.iter().filter(|c| !c.muted && c.pattern < p.patterns.len()).collect();
    placed.sort_by_key(|c| c.start);
    let fallback;
    if placed.is_empty() && p.patterns.get(p.current_pattern).is_some() {
        // A project played in pattern mode: its pattern at the start of the song.
        let pat = &p.patterns[p.current_pattern];
        fallback = ClipDef { pattern: p.current_pattern, lane: 0, start: 0, len: pat.len, offset: 0, muted: false };
        placed.push(&fallback);
    }
    for clip in placed {
        let pat = &p.patterns[clip.pattern];
        for (ch, list) in clips.iter_mut().enumerate() {
            let notes = clip_notes(p, clip, ch);
            if notes.is_empty() {
                continue;
            }
            // Clips on one track cannot overlap: the later one wins its place.
            if let Some(prev) = list.last_mut()
                && prev.start + prev.len > clip.start
            {
                prev.len = clip.start - prev.start;
                prev.notes.retain(|n| n.start < prev.len);
                for n in &mut prev.notes {
                    n.len = n.len.min(prev.len - n.start).max(1);
                }
                if prev.len == 0 {
                    list.pop();
                }
            }
            list.push(LiveClip { start: clip.start, len: clip.len, name: pat.name.clone(), color: nearest_color(pat.color), notes });
        }
    }

    x.open("Tracks", &[]);
    for (i, c) in p.channels.iter().enumerate() {
        let color = nearest_color(if c.color != 0 { c.color } else { p.tracks.get(c.track).map_or(0, |t| t.color) });
        let gain = if c.muted { 0.0003162278 } else { c.gain };
        midi_track(&mut x, i + 8, (&c.name, color, gain, c.pan), &clips[i], scenes);
    }
    x.close();
    let sig = p.signatures.iter().find(|s| s.tick == 0).map_or(TimeSig::FOUR_FOUR, |s| *s);
    main_track(&mut x, "MasterTrack", p.bpm, signature_value(sig.num, sig.den));
    main_track(&mut x, "PreHearTrack", p.bpm, 0);
    x.open("SendsPre", &[]);
    x.close();
    x.open("Scenes", &[]);
    for s in 0..scenes {
        x.open("Scene", &[("Id", &s.to_string())]);
        x.open("FollowAction", &[]);
        x.v("FollowTime", 4);
        x.v("IsLinked", "true");
        x.v("LoopIterations", 1);
        x.v("FollowActionA", 4);
        x.v("FollowActionB", 0);
        x.v("FollowChanceA", 100);
        x.v("FollowChanceB", 0);
        x.v("JumpIndexA", 0);
        x.v("JumpIndexB", 0);
        x.v("FollowActionEnabled", "false");
        x.close();
        x.v("Name", "");
        x.v("Annotation", "");
        x.v("Color", -1);
        x.v("Tempo", 120);
        x.v("IsTempoEnabled", "false");
        x.v("TimeSignatureId", 201);
        x.v("IsTimeSignatureEnabled", "false");
        x.v("LomId", 0);
        x.leaf("ClipSlotsListWrapper", &[("LomId", "0")]);
        x.close();
    }
    x.close();
    let song_end = p.clips.iter().map(|c| c.start + c.len).max().unwrap_or(TICKS_PER_BAR * 4);
    x.open("Transport", &[]);
    x.v("PhaseNudgeTempo", 10);
    x.v("LoopOn", "false");
    x.v("LoopStart", 0);
    x.v("LoopLength", beats(song_end.max(TICKS_PER_BAR)));
    x.v("LoopIsSongStart", "false");
    x.v("CurrentTime", 0);
    x.v("PunchIn", "false");
    x.v("PunchOut", "false");
    x.v("MetronomeTickDuration", 0);
    x.v("DrawMode", "false");
    x.close();
    x.open("SongMasterValues", &[]);
    x.leaf("SessionScrollerPos", &[("X", "0"), ("Y", "0")]);
    x.close();
    x.v("GlobalQuantisation", 4);
    x.v("AutoQuantisation", 0);
    x.open("Grid", &[]);
    x.v("FixedNumerator", 1);
    x.v("FixedDenominator", 16);
    x.v("GridIntervalPixel", 20);
    x.v("Ntoles", 2);
    x.v("SnapToGrid", "true");
    x.v("Fixed", "false");
    x.close();
    x.open("ScaleInformation", &[]);
    x.v("RootNote", p.key % 12);
    x.v("Name", "Major");
    x.close();
    x.v("InKey", "false");
    x.v("SmpteFormat", 0);
    x.open("TimeSelection", &[]);
    x.v("AnchorTime", 0);
    x.v("OtherTime", 0);
    x.close();
    x.open("Locators", &[]);
    x.open("Locators", &[]);
    for (i, m) in p.markers.iter().enumerate() {
        x.open("Locator", &[("Id", &i.to_string())]);
        x.v("LomId", 0);
        x.v("Time", beats(m.tick));
        x.v("Name", &m.name);
        x.v("Annotation", "");
        x.v("IsSongStart", "false");
        x.close();
    }
    x.close();
    x.close();
    x.open("DetailClipKeyMidis", &[]);
    x.close();
    for w in ["TracksListWrapper", "VisibleTracksListWrapper", "ReturnTracksListWrapper", "ScenesListWrapper", "CuePointsListWrapper"] {
        x.leaf(w, &[("LomId", "0")]);
    }
    x.v("ChooserBar", 0);
    x.v("Annotation", "");
    x.v("SoloOrPflSavedValue", "true");
    x.v("SoloInPlace", "true");
    x.v("CrossfadeCurve", 2);
    x.v("LatencyCompensation", 2);
    x.v("HighlightedTrackIndex", 0);
    x.open("GroovePool", &[]);
    x.v("LomId", 0);
    x.open("Grooves", &[]);
    x.close();
    x.close();
    x.v("AutomationMode", "false");
    x.v("SnapAutomationToGrid", "true");
    x.v("ArrangementOverdub", "false");
    x.v("ColorSequenceIndex", 0);
    x.open("AutoColorPickerForPlayerAndGroupTracks", &[]);
    x.v("NextColorIndex", 0);
    x.close();
    x.open("AutoColorPickerForReturnAndMasterTracks", &[]);
    x.v("NextColorIndex", 0);
    x.close();
    x.v("ViewData", "{}");
    x.v("MidiFoldIn", "false");
    x.v("MidiFoldMode", 0);
    x.v("MultiClipFocusMode", "false");
    x.v("MultiClipLoopBarHeight", 0);
    x.v("MidiPrelisten", "false");
    x.open("LinkedTrackGroups", &[]);
    x.close();
    x.v("AccidentalSpellingPreference", 3);
    x.v("PreferFlatRootNote", "false");
    x.v("UseWarperLegacyHiQMode", "false");
    x.leaf("VideoWindowRect", &[("Top", "-2147483648"), ("Left", "-2147483648"), ("Bottom", "-2147483648"), ("Right", "-2147483648")]);
    x.v("ShowVideoWindow", "true");
    x.v("TrackHeaderWidth", 93);
    x.open("ViewStates", &[]);
    x.v("SessionIO", 1);
    x.v("SessionSends", 1);
    x.v("SessionReturns", 1);
    x.v("SessionMixer", 1);
    x.v("SessionTrackDelay", 0);
    x.v("SessionCrossFade", 0);
    x.v("SessionShowOverView", 0);
    x.v("ArrangerIO", 1);
    x.v("ArrangerReturns", 1);
    x.v("ArrangerMixer", 1);
    x.v("ArrangerTrackDelay", 0);
    x.v("ArrangerShowOverView", 1);
    x.close();
    x.close();
    x.close();
    let next = (x.pointee + 1).to_string();
    x.out.replacen("@NEXT@", &next, 1)
}

/// A Live set (gzipped XML) for `project`.
pub fn export_als(project: &Project) -> Vec<u8> {
    crate::gzip::gzip(export_als_xml(project).as_bytes())
}

// --- Reading ---

/// An XML element: its name, attributes, child elements and text (the
/// pieces of text directly inside it, trimmed and joined; Live writes none,
/// MusicXML and Ardour do).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Element {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Element>,
    pub text: String,
}

impl Element {
    pub fn attr(&self, key: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    pub fn child(&self, name: &str) -> Option<&Element> {
        self.children.iter().find(|c| c.name == name)
    }

    /// The element at the end of a path of child names.
    pub fn at(&self, path: &[&str]) -> Option<&Element> {
        path.iter().try_fold(self, |e, name| e.child(name))
    }

    /// `Value` of the element at `path`.
    pub fn value(&self, path: &[&str]) -> Option<&str> {
        self.at(path)?.attr("Value")
    }

    pub(crate) fn num(&self, path: &[&str]) -> Option<f64> {
        self.value(path)?.trim().parse().ok().filter(|v: &f64| v.is_finite())
    }

    /// The children named `name`.
    pub fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Element> + 'a {
        self.children.iter().filter(move |c| c.name == name)
    }

    /// Trimmed text of the child `name`.
    pub fn child_text(&self, name: &str) -> Option<&str> {
        self.child(name).map(|c| c.text.trim())
    }

    /// Every descendant named `name`, depth first (not inside a match).
    pub fn find_all<'a>(&'a self, name: &str, out: &mut Vec<&'a Element>) {
        for c in &self.children {
            if c.name == name {
                out.push(c);
            } else {
                c.find_all(name, out);
            }
        }
    }
}

fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(end) = rest.find(';').filter(|&e| e <= 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let ent = &rest[1..end];
        let c = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => ent
                .strip_prefix("#x")
                .map(|h| u32::from_str_radix(h, 16))
                .or_else(|| ent.strip_prefix('#').map(|d| d.parse::<u32>()))
                .and_then(|r| r.ok())
                .and_then(char::from_u32),
        };
        match c {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Adds a piece of text to an element (whitespace between tags is dropped).
fn push_text(el: &mut Element, piece: &str) {
    let piece = piece.trim();
    if piece.is_empty() {
        return;
    }
    if !el.text.is_empty() {
        el.text.push(' ');
    }
    el.text.push_str(piece);
}

/// Parses an XML document into its root element. Tolerant: comments,
/// processing instructions and DOCTYPEs are skipped; unclosed elements close
/// at the end. Text is kept (see [`Element::text`]), CDATA sections too.
pub fn parse_xml(text: &str) -> Result<Element, String> {
    let b = text.as_bytes();
    let mut stack: Vec<Element> = vec![Element::default()];
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'<' {
            let end = text[i..].find('<').map_or(b.len(), |e| i + e);
            if stack.len() > 1 {
                push_text(stack.last_mut().expect("root"), &unescape(&text[i..end]));
            }
            i = end;
            continue;
        }
        if let Some(rest) = text[i..].strip_prefix("<![CDATA[") {
            let len = rest.find("]]>").unwrap_or(rest.len());
            if stack.len() > 1 {
                push_text(stack.last_mut().expect("root"), &rest[..len]);
            }
            i = (i + 9 + len + 3).min(b.len());
            continue;
        }
        if text[i..].starts_with("<!--") {
            i = text[i..].find("-->").map_or(b.len(), |e| i + e + 3);
            continue;
        }
        if text[i..].starts_with("<?") || text[i..].starts_with("<!") {
            i = text[i..].find('>').map_or(b.len(), |e| i + e + 1);
            continue;
        }
        if text[i..].starts_with("</") {
            i = text[i..].find('>').map_or(b.len(), |e| i + e + 1);
            if stack.len() > 1 {
                let done = stack.pop().expect("checked");
                stack.last_mut().expect("root").children.push(done);
            }
            continue;
        }
        // A start tag: name, attributes, maybe "/>".
        i += 1;
        let start = i;
        while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'>' && b[i] != b'/' {
            i += 1;
        }
        let mut el = Element { name: text[start..i].to_string(), ..Default::default() };
        let mut closed = false;
        loop {
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            if i >= b.len() {
                break;
            }
            match b[i] {
                b'>' => {
                    i += 1;
                    break;
                }
                b'/' => {
                    closed = true;
                    i += 1;
                }
                _ => {
                    let k0 = i;
                    while i < b.len() && b[i] != b'=' && !b[i].is_ascii_whitespace() && b[i] != b'>' && b[i] != b'/' {
                        i += 1;
                    }
                    let key = text[k0..i].to_string();
                    while i < b.len() && b[i].is_ascii_whitespace() {
                        i += 1;
                    }
                    if b.get(i) != Some(&b'=') {
                        el.attrs.push((key, String::new()));
                        continue;
                    }
                    i += 1;
                    while i < b.len() && b[i].is_ascii_whitespace() {
                        i += 1;
                    }
                    let quote = *b.get(i).ok_or("truncated XML")?;
                    if quote != b'"' && quote != b'\'' {
                        return Err(tr("Damaged XML: an attribute without quotes").into());
                    }
                    let v0 = i + 1;
                    let end = text[v0..].find(quote as char).ok_or(tr("Damaged XML: an attribute is not closed"))? + v0;
                    el.attrs.push((key, unescape(&text[v0..end])));
                    i = end + 1;
                }
            }
        }
        if el.name.is_empty() {
            continue;
        }
        if closed {
            stack.last_mut().expect("root").children.push(el);
        } else {
            stack.push(el);
        }
    }
    while stack.len() > 1 {
        let done = stack.pop().expect("checked");
        stack.last_mut().expect("root").children.push(done);
    }
    stack.pop().expect("root").children.into_iter().next().ok_or_else(|| tr("Empty XML document").into())
}

/// The closest Ponzik instrument for a Live track's first instrument device
/// (or its name).
fn instrument_for_track(track: &Element, name: &str) -> &'static str {
    let mut devices = Vec::new();
    if let Some(chain) = track.at(&["DeviceChain", "DeviceChain", "Devices"]) {
        devices.extend(chain.children.iter());
    }
    for d in devices {
        let kind = match d.name.as_str() {
            "DrumGroupDevice" => "drum_machine",
            "OriginalSimpler" | "MultiSampler" => "sampler",
            "Operator" => "fm",
            "InstrumentVector" => "wavetable",
            "LoungeLizard" => "epiano",
            "Collision" => "mallets",
            "StringStudio" => "guitar",
            "UltraAnalog" | "InstrumentMeld" | "Drift" => "pulse",
            "PluginDevice" | "AuPluginDevice" => {
                let plug = d
                    .value(&["PluginDesc", "VstPluginInfo", "PlugName"])
                    .or_else(|| d.value(&["PluginDesc", "Vst3PluginInfo", "Name"]))
                    .or_else(|| d.value(&["PluginDesc", "AuPluginInfo", "Name"]))
                    .unwrap_or(name);
                crate::flp::instrument_for(plug)
            }
            "InstrumentGroupDevice" => crate::flp::instrument_for(name),
            _ => continue,
        };
        return kind;
    }
    crate::flp::instrument_for(name)
}

fn track_name(track: &Element, fallback: String) -> String {
    let user = track.value(&["Name", "UserName"]).filter(|s| !s.trim().is_empty());
    let effective = track.value(&["Name", "EffectiveName"]).filter(|s| !s.trim().is_empty());
    user.or(effective).map_or(fallback, str::to_string)
}

fn track_color(track: &Element) -> u32 {
    let idx = track.value(&["Color"]).or_else(|| track.value(&["ColorIndex"])).and_then(|v| v.parse::<usize>().ok());
    idx.map_or(0, |i| PALETTE[i % PALETTE.len()])
}

/// A MIDI clip of a Live set, read: its place in beats and its notes.
struct ReadClip {
    time: f64,
    /// Length in the arrangement (beats).
    len: f64,
    /// Content played: from `content_start`, looping over `loop_len` if looped.
    content_start: f64,
    loop_len: Option<f64>,
    /// Where playback starts inside the loop (looped clips).
    start_rel: f64,
    name: String,
    color: u32,
    /// (time, duration, key, velocity 0-127), in clip time.
    notes: Vec<(f64, f64, u8, f64)>,
}

fn read_midi_clip(c: &Element) -> ReadClip {
    let time = c.attr("Time").and_then(|t| t.parse().ok()).filter(|t: &f64| t.is_finite()).unwrap_or(0.0);
    let start = c.num(&["CurrentStart"]).unwrap_or(time);
    let end = c.num(&["CurrentEnd"]).unwrap_or(start + 4.0);
    let loop_on = c.value(&["Loop", "LoopOn"]) == Some("true");
    let loop_start = c.num(&["Loop", "LoopStart"]).unwrap_or(0.0);
    let loop_end = c.num(&["Loop", "LoopEnd"]).unwrap_or(loop_start + (end - start));
    let mut notes = Vec::new();
    if let Some(tracks) = c.at(&["Notes", "KeyTracks"]) {
        for kt in tracks.children.iter().filter(|k| k.name == "KeyTrack") {
            let Some(key) = kt.num(&["MidiKey"]).filter(|k| (0.0..128.0).contains(k)) else { continue };
            for n in kt.child("Notes").map_or(&[][..], |n| &n.children[..]) {
                let num = |k: &str| n.attr(k).and_then(|v| v.trim().parse::<f64>().ok()).filter(|v| v.is_finite());
                if n.attr("IsEnabled") == Some("false") {
                    continue;
                }
                let (Some(t), Some(d)) = (num("Time"), num("Duration")) else { continue };
                notes.push((t, d, key as u8, num("Velocity").unwrap_or(100.0)));
            }
        }
    }
    ReadClip {
        time,
        len: (end - start).max(0.0),
        content_start: loop_start,
        loop_len: loop_on.then_some(loop_end - loop_start).filter(|l| *l > 0.0),
        start_rel: c.num(&["Loop", "StartRelative"]).unwrap_or(0.0),
        name: c.value(&["Name"]).unwrap_or("").to_string(),
        color: c.value(&["Color"]).and_then(|v| v.parse::<usize>().ok()).map_or(0, |i| PALETTE[i % PALETTE.len()]),
        notes,
    }
}

/// Pattern content of a clip: its notes from the content start, and the
/// pattern's length; for looped clips the clip's offset into the loop.
fn clip_pattern(c: &ReadClip) -> (Vec<Note>, u32, u32) {
    let (window, offset) = match c.loop_len {
        Some(l) => (l, ticks(c.start_rel)),
        None => (c.len, 0),
    };
    let len = ticks(window).max(1);
    let mut notes: Vec<Note> = c
        .notes
        .iter()
        .filter_map(|&(t, d, key, vel)| {
            let at = t - c.content_start;
            if at < -1e-6 || ticks(at.max(0.0)) >= len {
                return None;
            }
            let start = ticks(at.max(0.0));
            let nl = ticks(d).clamp(1, len - start);
            Some(Note { start, len: nl, key, vel: (vel / 127.0).clamp(0.0, 1.0) as f32, ..Note::DEFAULT })
        })
        .collect();
    notes.sort_by_key(|n| (n.start, n.key));
    (notes, len, offset)
}

/// Converts a Live set (gzipped or plain XML) into a new Ponzik project.
/// `resolve` turns an audio file path of the set into a readable path.
pub fn import_als(bytes: &[u8], name: &str, resolve: &dyn Fn(&str) -> Option<String>) -> Result<(Project, ImportReport), String> {
    let xml = if crate::gzip::is_gzip(bytes) {
        crate::gzip::gunzip(bytes, MAX_XML).map_err(|e| tr_fmt!("Not a readable Ableton Live set: {}", e))?
    } else {
        bytes.to_vec()
    };
    let text = String::from_utf8_lossy(&xml);
    let root = parse_xml(&text)?;
    if root.name != "Ableton" {
        return Err(tr("Not an Ableton Live set").into());
    }
    let set = root.child("LiveSet").ok_or(tr("Not an Ableton Live set"))?;
    let mut report = ImportReport { source: "Ableton Live".into(), ..Default::default() };
    let mut p = Project::empty();
    p.name = name.to_string();
    p.patterns.clear();
    if let Some(v) = root.attr("Creator") {
        report.mapped.push(tr_fmt!("Set saved by {}", v));
    }

    // Tempo and time signature (Live 12 calls the master track "MainTrack").
    let master = set.child("MasterTrack").or_else(|| set.child("MainTrack"));
    let mixer = master.and_then(|m| m.at(&["DeviceChain", "Mixer"]));
    match mixer.and_then(|m| m.num(&["Tempo", "Manual"])) {
        Some(bpm) if bpm > 0.0 => {
            p.bpm = (bpm as f32).clamp(10.0, 999.0);
            report.mapped.push(tr_fmt!("Tempo {:.2} BPM", p.bpm));
        }
        _ => report.approximated.push(tr("No tempo found: 120 BPM").into()),
    }
    if let Some(sig) = mixer.and_then(|m| m.num(&["TimeSignature", "Manual"])) {
        let (num, den) = signature_from(sig.max(0.0) as u32);
        if (num, den) != (4, 4) {
            p.signatures.push(TimeSig { tick: 0, num, den });
            report.mapped.push(tr_fmt!("Time signature {}/{}", num, den));
        }
    }

    let tracks = set.child("Tracks").map_or(&[][..], |t| &t.children[..]);
    let (mut midi_clips, mut session_clips, mut audio_count, mut notes_count) = (0, 0, 0, 0);
    let mut skipped_tracks = Vec::new();
    // Identical clips share one pattern: (channel, notes, length) → pattern.
    let mut seen: Vec<(usize, Vec<Note>, u32, usize)> = Vec::new();
    let mut lane = 0;
    // Mixer tracks in the order of the set's tracks, from track 1.
    let mut next_track = 1;
    let mut take_track = |p: &mut Project, name: &str, color: u32| -> usize {
        while p.tracks.len() <= next_track {
            if p.add_track().is_none() {
                return ponzik_engine::mixer::MASTER;
            }
        }
        let t = next_track;
        next_track += 1;
        p.tracks[t].name = name.to_string();
        p.tracks[t].color = color;
        t
    };
    for (ti, track) in tracks.iter().enumerate() {
        let label = track_name(track, tr_fmt!("Track {}", ti + 1));
        match track.name.as_str() {
            "MidiTrack" => {
                let ch = p.channels.len();
                let kind = instrument_for_track(track, &label);
                let mixer = track.at(&["DeviceChain", "Mixer"]);
                let gain = mixer.and_then(|m| m.num(&["Volume", "Manual"])).unwrap_or(1.0) as f32;
                let pan = mixer.and_then(|m| m.num(&["Pan", "Manual"])).unwrap_or(0.0) as f32;
                let color = track_color(track);
                let track_slot = take_track(&mut p, &label, color);
                p.channels.push(ChannelDef {
                    name: label.clone(),
                    instrument: InstrumentDef::new(kind),
                    note_fx: Vec::new(),
                    gain: gain.clamp(0.0, 1.25),
                    pan: pan.clamp(-1.0, 1.0),
                    muted: false,
                    track: track_slot,
                    root_note: 60,
                    color,
                    modulators: Vec::new(),
                    frozen: None,
                    porta: 0.0,
                });
                let mut found = Vec::new();
                if let Some(events) = track.at(&["DeviceChain", "MainSequencer", "ClipTimeable", "ArrangerAutomation", "Events"]) {
                    events.find_all("MidiClip", &mut found);
                }
                let arranged = found.len();
                if let Some(slots) = track.at(&["DeviceChain", "MainSequencer", "ClipSlotList"]) {
                    slots.find_all("MidiClip", &mut found);
                }
                for (k, el) in found.iter().enumerate() {
                    let clip = read_midi_clip(el);
                    let (notes, len, offset) = clip_pattern(&clip);
                    notes_count += notes.len();
                    let pattern = match seen.iter().find(|s| s.0 == ch && s.1 == notes && s.2 == len) {
                        Some(s) => s.3,
                        None => {
                            let name = if clip.name.trim().is_empty() { label.clone() } else { clip.name.clone() };
                            let mut per = vec![Vec::new(); ch + 1];
                            per[ch] = notes.clone();
                            p.patterns.push(PatternDef { name, len, notes: per, color: clip.color, groove: None });
                            seen.push((ch, notes, len, p.patterns.len() - 1));
                            p.patterns.len() - 1
                        }
                    };
                    if k < arranged {
                        let start = ticks(clip.time);
                        if start < MAX_TICK {
                            p.clips.push(ClipDef { pattern, lane, start, len: ticks(clip.len).max(1), offset, muted: false });
                            midi_clips += 1;
                        }
                    } else {
                        session_clips += 1;
                    }
                }
                lane += 1;
            }
            "AudioTrack" => {
                let mixer = track.at(&["DeviceChain", "Mixer"]);
                let track_slot = take_track(&mut p, &label, track_color(track));
                p.tracks[track_slot].gain = mixer.and_then(|m| m.num(&["Volume", "Manual"])).unwrap_or(1.0).clamp(0.0, 1.25) as f32;
                p.tracks[track_slot].pan = mixer.and_then(|m| m.num(&["Pan", "Manual"])).unwrap_or(0.0).clamp(-1.0, 1.0) as f32;
                let mut found = Vec::new();
                if let Some(events) = track.at(&["DeviceChain", "MainSequencer", "Sample", "ArrangerAutomation", "Events"]) {
                    events.find_all("AudioClip", &mut found);
                }
                for el in found {
                    let raw = el
                        .value(&["SampleRef", "FileRef", "Path"])
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .or_else(|| el.value(&["SampleRef", "FileRef", "RelativePath"]).map(str::to_string));
                    let Some(raw) = raw else { continue };
                    let path = resolve(&raw).unwrap_or(raw.clone());
                    if resolve(&raw).is_none() {
                        report.offline.push(raw.clone());
                    }
                    let time = el.attr("Time").and_then(|t| t.parse::<f64>().ok()).unwrap_or(0.0);
                    let start = el.num(&["CurrentStart"]).unwrap_or(time);
                    let end = el.num(&["CurrentEnd"]).unwrap_or(start + 4.0);
                    p.audio_clips.push(AudioClipDef {
                        path,
                        lane,
                        track: track_slot,
                        start: ticks(time),
                        len: ticks(end - start).max(1),
                        gain: 1.0,
                        ..Default::default()
                    });
                    audio_count += 1;
                }
                lane += 1;
            }
            "GroupTrack" | "ReturnTrack" => skipped_tracks.push(label),
            _ => {}
        }
    }
    for pat in &mut p.patterns {
        pat.notes.resize(p.channels.len(), Vec::new());
    }
    if p.patterns.is_empty() {
        p.patterns.push(Project::empty().patterns.remove(0));
    }
    if !p.channels.is_empty() {
        report.mapped.push(tr_fmt!("{} MIDI tracks: name, colour, volume, pan (instruments guessed from their devices)", p.channels.len()));
    }
    if midi_clips + session_clips > 0 {
        report.mapped.push(tr_fmt!("{} patterns from {} MIDI clips, {} notes", p.patterns.len(), midi_clips + session_clips, notes_count));
    }
    if session_clips > 0 {
        report.approximated.push(tr_fmt!("{} Session view clips became patterns, not placed in the playlist", session_clips));
    }
    if audio_count > 0 {
        report.approximated.push(tr_fmt!("{} audio clips: file, place and length (warping and the start in the file are not imported)", audio_count));
    }
    if !skipped_tracks.is_empty() {
        report.skipped.push(tr_fmt!("Group and return tracks: {}", skipped_tracks.join(", ")));
    }
    if let Some(locs) = set.at(&["Locators", "Locators"]) {
        for l in locs.children.iter().filter(|l| l.name == "Locator") {
            let tick = l.num(&["Time"]).map_or(0, ticks);
            p.markers.push(Marker { tick, name: l.value(&["Name"]).unwrap_or("").to_string(), color: 0 });
        }
        p.markers.sort_by_key(|m| m.tick);
        p.markers.dedup_by_key(|m| m.tick);
        if !p.markers.is_empty() {
            report.mapped.push(tr_fmt!("{} locators as arrangement markers", p.markers.len()));
        }
    }
    report.approximated.push(tr("Live devices, their settings, automation and routing are not converted").into());
    if !p.clips.is_empty() || !p.audio_clips.is_empty() {
        p.mode = PlayMode::Song;
    }
    Ok((p, report))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_parses_attributes_entities_and_nesting() {
        let doc = r#"<?xml version="1.0"?><!-- hi --><A x="1 &amp; 2" y='&lt;b&gt;'><B Value="&#233;t&#xE9;"/><C><D Value="4" /></C></A>"#;
        let a = parse_xml(doc).unwrap();
        assert_eq!(a.name, "A");
        assert_eq!(a.attr("x"), Some("1 & 2"));
        assert_eq!(a.attr("y"), Some("<b>"));
        assert_eq!(a.value(&["B"]), Some("été"));
        assert_eq!(a.value(&["C", "D"]), Some("4"));
        assert!(parse_xml("").is_err());
        assert!(parse_xml("<A x=1/>").is_err());
        // Unclosed elements close at the end.
        assert_eq!(parse_xml("<A><B>").unwrap().children.len(), 1);
        // Text, entities and CDATA.
        let t = parse_xml("<a> <s>C</s>\n <o>4 &amp; 5</o><c><![CDATA[x<y]]></c></a>").unwrap();
        assert_eq!(t.child_text("s"), Some("C"));
        assert_eq!(t.child_text("o"), Some("4 & 5"));
        assert_eq!(t.child_text("c"), Some("x<y"));
        assert_eq!(t.text, "");
        assert_eq!(t.children_named("s").count(), 1);
    }

    #[test]
    fn signatures_use_lives_encoding() {
        assert_eq!(signature_value(4, 4), 201);
        assert_eq!(signature_from(201), (4, 4));
        assert_eq!(signature_from(signature_value(7, 8)), (7, 8));
        assert_eq!(signature_from(signature_value(3, 4)), (3, 4));
    }

    #[test]
    fn export_is_a_gzipped_live_set() {
        let mut p = crate::demo_project();
        p.name = "Set & <test>".into();
        p.markers.push(Marker { tick: TICKS_PER_BAR * 2, name: "Drop".into(), color: 0 });
        let bytes = export_als(&p);
        let xml = crate::gzip::gunzip(&bytes, MAX_XML).expect("valid gzip with its CRC");
        let text = String::from_utf8(xml).unwrap();
        assert!(text.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Ableton MajorVersion=\"5\""));
        let root = parse_xml(&text).unwrap();
        let set = root.child("LiveSet").unwrap();
        // One MIDI track per channel, named.
        let tracks: Vec<&Element> = set.child("Tracks").unwrap().children.iter().collect();
        assert_eq!(tracks.len(), p.channels.len());
        for (t, c) in tracks.iter().zip(&p.channels) {
            assert_eq!(t.name, "MidiTrack");
            assert_eq!(t.value(&["Name", "EffectiveName"]), Some(c.name.as_str()));
        }
        assert_eq!(set.value(&["MasterTrack", "DeviceChain", "Mixer", "Tempo", "Manual"]).unwrap().parse::<f32>().unwrap(), p.bpm);
        // Every automation target id is unique and below NextPointeeId.
        let mut ids = Vec::new();
        fn collect(e: &Element, ids: &mut Vec<u32>) {
            if e.name.ends_with("Target") || e.name.starts_with("ControllerTargets.") || e.name == "Pointee" {
                ids.extend(e.attr("Id").and_then(|v| v.parse::<u32>().ok()));
            }
            for c in &e.children {
                collect(c, ids);
            }
        }
        collect(set, &mut ids);
        let next: u32 = set.value(&["NextPointeeId"]).unwrap().parse().unwrap();
        let n = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), n, "unique ids");
        assert!(ids.iter().all(|&i| i < next));
        // Notes count: every playlist clip's notes, unrolled.
        let mut notes = Vec::new();
        set.find_all("MidiNoteEvent", &mut notes);
        let want: usize = p.clips.iter().filter(|c| !c.muted).map(|c| (0..p.channels.len()).map(|ch| clip_notes(&p, c, ch).len()).sum::<usize>()).sum();
        assert_eq!(notes.len(), want);
        assert!(want > 0);
        assert_eq!(set.value(&["Locators", "Locators", "Locator", "Name"]), Some("Drop"));
    }

    #[test]
    fn a_project_comes_back_through_the_importer() {
        let p = crate::demo_project();
        let (q, report) = import_als(&export_als(&p), "Back", &|s| Some(s.to_string())).unwrap();
        assert_eq!(report.source, "Ableton Live");
        assert!((q.bpm - p.bpm).abs() < 1e-3);
        assert_eq!(q.channels.iter().map(|c| c.name.clone()).collect::<Vec<_>>(), p.channels.iter().map(|c| c.name.clone()).collect::<Vec<_>>());
        // The song plays the same notes at the same times, channel by channel.
        let song = |p: &Project| {
            let mut v = Vec::new();
            for c in p.clips.iter().filter(|c| !c.muted) {
                for ch in 0..p.channels.len() {
                    for n in clip_notes(p, c, ch) {
                        v.push((ch, c.start + n.start, n.key, n.len));
                    }
                }
            }
            v.sort_unstable();
            v
        };
        assert_eq!(song(&q), song(&p));
        assert_eq!(q.mode, PlayMode::Song);
    }

    #[test]
    fn looped_clips_audio_and_session_clips_import() {
        let set = r#"<?xml version="1.0" encoding="UTF-8"?>
<Ableton MajorVersion="5" MinorVersion="12.0_12049" Creator="Ableton Live 12.0">
 <LiveSet>
  <Tracks>
   <MidiTrack Id="1">
    <Name><EffectiveName Value="1-Bass" /><UserName Value="" /></Name>
    <Color Value="5" />
    <DeviceChain>
     <Mixer><Volume><Manual Value="0.5" /></Volume><Pan><Manual Value="-0.5" /></Pan></Mixer>
     <MainSequencer>
      <ClipSlotList><ClipSlot Id="0"><ClipSlot><Value>
       <MidiClip Id="0" Time="0"><CurrentStart Value="0" /><CurrentEnd Value="4" />
        <Loop><LoopStart Value="0" /><LoopEnd Value="4" /><LoopOn Value="true" /></Loop>
        <Notes><KeyTracks><KeyTrack Id="0"><Notes><MidiNoteEvent Time="0" Duration="1" Velocity="64" IsEnabled="true" /></Notes><MidiKey Value="40" /></KeyTrack></KeyTracks></Notes>
       </MidiClip></Value></ClipSlot></ClipSlot></ClipSlotList>
      <ClipTimeable><ArrangerAutomation><Events>
       <MidiClip Id="1" Time="8"><CurrentStart Value="8" /><CurrentEnd Value="16" /><Name Value="Riff" />
        <Loop><LoopStart Value="1" /><LoopEnd Value="3" /><StartRelative Value="1" /><LoopOn Value="true" /></Loop>
        <Notes><KeyTracks><KeyTrack Id="0"><Notes>
         <MidiNoteEvent Time="1" Duration="0.5" Velocity="127" IsEnabled="true" />
         <MidiNoteEvent Time="2.5" Duration="0.25" Velocity="100" IsEnabled="false" />
         <MidiNoteEvent Time="0" Duration="0.25" Velocity="100" IsEnabled="true" />
        </Notes><MidiKey Value="45" /></KeyTrack></KeyTracks></Notes>
       </MidiClip>
      </Events></ArrangerAutomation></ClipTimeable>
     </MainSequencer>
     <DeviceChain><Devices><Operator Id="0" /></Devices></DeviceChain>
    </DeviceChain>
   </MidiTrack>
   <AudioTrack Id="2">
    <Name><EffectiveName Value="Vox" /></Name>
    <DeviceChain><MainSequencer><Sample><ArrangerAutomation><Events>
     <AudioClip Id="0" Time="4"><CurrentStart Value="4" /><CurrentEnd Value="12" />
      <SampleRef><FileRef><Path Value="/music/vox.wav" /></FileRef></SampleRef></AudioClip>
    </Events></ArrangerAutomation></Sample></MainSequencer></DeviceChain>
   </AudioTrack>
   <ReturnTrack Id="3"><Name><EffectiveName Value="A-Reverb" /></Name></ReturnTrack>
  </Tracks>
  <MainTrack><DeviceChain><Mixer><Tempo><Manual Value="92" /></Tempo><TimeSignature><Manual Value="200" /></TimeSignature></Mixer></DeviceChain></MainTrack>
  <Locators><Locators><Locator Id="0"><Time Value="8" /><Name Value="B" /></Locator></Locators></Locators>
 </LiveSet>
</Ableton>"#;
        let (p, report) = import_als(set.as_bytes(), "Set", &|_| None).unwrap();
        assert_eq!(p.bpm, 92.0);
        assert_eq!(p.signatures, vec![TimeSig { tick: 0, num: 3, den: 4 }]);
        assert_eq!(p.channels.len(), 1);
        let c = &p.channels[0];
        assert_eq!((c.name.as_str(), c.instrument.kind.as_str()), ("1-Bass", "fm"));
        assert!((c.gain - 0.5).abs() < 1e-6 && (c.pan + 0.5).abs() < 1e-6);
        assert_eq!(c.color, PALETTE[5]);
        // The looped arrangement clip: a 2-beat pattern from beat 1, entered 1 beat in.
        assert_eq!(p.clips.len(), 1);
        let clip = p.clips[0];
        assert_eq!((clip.start, clip.len, clip.offset), (8 * PPQ, 8 * PPQ, PPQ));
        let pat = &p.patterns[clip.pattern];
        assert_eq!(pat.name, "Riff");
        assert_eq!(pat.len, 2 * PPQ);
        assert_eq!(pat.notes[0].iter().map(|n| (n.start, n.len, n.key)).collect::<Vec<_>>(), vec![(0, PPQ / 2, 45)]);
        // The Session clip is a pattern of its own.
        assert_eq!(p.patterns.len(), 2);
        assert!(report.approximated.iter().any(|s| s.contains("Session")));
        // The audio clip, offline here.
        assert_eq!(p.audio_clips.len(), 1);
        let a = &p.audio_clips[0];
        assert_eq!((a.path.as_str(), a.start, a.len, a.lane), ("/music/vox.wav", 4 * PPQ, 8 * PPQ, 1));
        assert_eq!(report.offline, vec!["/music/vox.wav".to_string()]);
        assert!(report.skipped.iter().any(|s| s.contains("A-Reverb")));
        assert_eq!(p.markers.len(), 1);
        assert_eq!(p.markers[0].tick, 8 * PPQ);
    }

    #[test]
    fn broken_sets_are_errors_not_panics() {
        assert!(import_als(b"", "x", &|_| None).is_err());
        assert!(import_als(&[0x1f, 0x8b, 8, 0, 1, 2], "x", &|_| None).is_err());
        assert!(import_als(b"<NotLive/>", "x", &|_| None).is_err());
        let (p, _) = import_als(b"<Ableton><LiveSet></LiveSet></Ableton>", "x", &|_| None).unwrap();
        assert!(p.channels.is_empty() && !p.patterns.is_empty());
    }
}
