//! FL Studio project import (`.flp`).
//!
//! The format is undocumented by Image-Line; everything here follows public
//! reverse-engineering work:
//!
//! - PyFLP by demberto (GPL-3.0), <https://github.com/demberto/PyFLP>:
//!   `pyflp/__init__.py` (container), `pyflp/_events.py` (event id ranges),
//!   `pyflp/project.py`, `pyflp/channel.py`, `pyflp/pattern.py`,
//!   `pyflp/arrangement.py`, `pyflp/mixer.py` and `pyflp/plugin.py` (event ids
//!   and struct layouts), and `docs/architecture/flp-format.rst`.
//! - Event ordering (which events open and close an insert or an effect slot)
//!   was checked against PyFLP's test project `tests/assets/FL 20.8.4.flp`
//!   (not committed here).
//!
//! The container is a `FLhd` chunk (format, channel count, PPQ) followed by a
//! `FLdt` chunk holding a flat stream of events. An event is an id byte and a
//! value whose size depends on the id: ids 0-63 hold one byte, 64-127 a 16-bit
//! word, 128-191 a 32-bit dword, 192-255 a varint length followed by that many
//! bytes (text or a struct). Text is UTF-16LE from FL Studio 11.5 on, ASCII
//! before.
//!
//! Only the arrangement-level data is converted: channels become Ponzik
//! channels with the closest native instrument, patterns keep their notes,
//! playlist clips keep their positions, mixer inserts keep their names,
//! levels and routing. Plugin settings, envelopes and automation clips are not
//! converted; [`ImportReport`] lists what was mapped, approximated or skipped.

use std::collections::BTreeMap;
use std::path::Path;

use ponzik_i18n::{tr, tr_fmt};

use ponzik_engine::mixer::{INSERT_SLOTS, MASTER, SEND_SLOTS, TRACKS};
use ponzik_engine::{Note, PPQ, PlayMode, TICKS_PER_BAR, TICKS_PER_STEP};

use crate::{AudioClipDef, ChannelDef, ClipDef, EffectDef, InstrumentDef, PatternDef, Project};

// --- Event ids (PyFLP `_events.py`: BYTE = 0, WORD = 64, DWORD = 128, TEXT = 192, DATA = 208) ---

const WORD: u8 = 64;
const DWORD: u8 = 128;
const TEXT: u8 = 192;
const DATA: u8 = 208;

// Project (PyFLP `project.py`, `ProjectID`; `arrangement.py`, `ArrangementsID`).
const TIME_SIG_NUM: u8 = 17;
const TIME_SIG_BEAT: u8 = 18;
const TEMPO_COARSE: u8 = WORD + 2;
const TEMPO_FINE: u8 = WORD + 29;
const TEMPO: u8 = DWORD + 28;
const TITLE: u8 = TEXT + 2;
const FL_VERSION: u8 = TEXT + 7;

// Channels (PyFLP `channel.py`, `ChannelID`).
const CH_ENABLED: u8 = 0;
const CH_VOL_BYTE: u8 = 2;
const CH_PAN_BYTE: u8 = 3;
const CH_TYPE: u8 = 21;
const CH_ROUTED_TO: u8 = 22;
const CH_NEW: u8 = WORD;
const CH_VOL_WORD: u8 = WORD + 8;
const CH_PAN_WORD: u8 = WORD + 9;
const CH_ROOT_NOTE: u8 = DWORD + 7;
const CH_NAME_OLD: u8 = TEXT;
const CH_SAMPLE_PATH: u8 = TEXT + 4;
const CH_LEVELS: u8 = DATA + 11;

// Channel types (PyFLP `channel.py`, `ChannelType`).
const TYPE_SAMPLER: u8 = 0;
const TYPE_NATIVE: u8 = 2;
const TYPE_LAYER: u8 = 3;
const TYPE_INSTRUMENT: u8 = 4;
const TYPE_AUTOMATION: u8 = 5;

// Plugins, shared by channels and mixer slots (PyFLP `plugin.py`, `PluginID`).
const PLUGIN_COLOR: u8 = DWORD;
const PLUGIN_INTERNAL_NAME: u8 = TEXT + 9;
const PLUGIN_NAME: u8 = TEXT + 11;

// Patterns (PyFLP `pattern.py`, `PatternID`).
const PAT_NEW: u8 = WORD + 1;
const PAT_COLOR: u8 = DWORD + 22;
const PAT_LENGTH: u8 = DWORD + 36;
const PAT_NAME: u8 = TEXT + 1;
const PAT_NOTES: u8 = DATA + 16;

// Arrangements and playlist (PyFLP `arrangement.py`, `ArrangementID`, `TrackID`).
const ARR_NEW: u8 = WORD + 35;
const ARR_CURRENT: u8 = WORD + 36;
const ARR_PLAYLIST: u8 = DATA + 25;
const ARR_NAME: u8 = TEXT + 49;
const TRACK_DATA: u8 = DATA + 30;
/// Playlist item index base: items above it are patterns (`PlaylistEvent.pattern_base`).
const PATTERN_BASE: u16 = 20480;

// Mixer (PyFLP `mixer.py`, `InsertID`, `MixerID`, `SlotID`). An insert's events
// run from its name/colour to its `INS_OUTPUT` event; each effect slot's plugin
// events come before its `SLOT_INDEX` event.
const SLOT_INDEX: u8 = WORD + 34;
const INS_OUTPUT: u8 = DWORD + 19;
const INS_COLOR: u8 = DWORD + 21;
const INS_NAME: u8 = TEXT + 12;
const MIXER_PARAMS: u8 = DATA + 17;
const INS_ROUTING: u8 = DATA + 27;
const INS_FLAGS: u8 = DATA + 28;

// Mixer parameter ids (PyFLP `mixer.py`, `_MixerParamsID`).
const PARAM_SLOT_ENABLED: u8 = 0;
const PARAM_ROUTE_START: u8 = 64;
const PARAM_VOLUME: u8 = 192;
const PARAM_PAN: u8 = 193;

/// Text ids above `DATA` that still hold strings (PyFLP `NEW_TEXT_IDS`).
const NEW_TEXT_IDS: [u8; 3] = [ARR_NAME, TEXT + 39, TEXT + 47];

/// Unity level of channel volumes and mixer faders (100 % in FL Studio).
const FL_UNITY: f32 = 12_800.0;
/// Clips and notes beyond this tick are dropped (garbage protection).
const MAX_TICK: u32 = TICKS_PER_BAR * 4096;

// --- Container ---

/// Contents of the `FLhd` chunk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub format: i16,
    pub channels: u16,
    /// Pulses per quarter note of every position in the file.
    pub ppq: u16,
}

/// Value of one event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Value<'a> {
    Byte(u8),
    Word(u16),
    Dword(u32),
    Data(&'a [u8]),
}

impl Value<'_> {
    /// Integer value (0 for data events).
    fn int(&self) -> u32 {
        match *self {
            Value::Byte(v) => v as u32,
            Value::Word(v) => v as u32,
            Value::Dword(v) => v,
            Value::Data(_) => 0,
        }
    }

    fn data(&self) -> &[u8] {
        match self {
            Value::Data(d) => d,
            _ => &[],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event<'a> {
    pub id: u8,
    pub value: Value<'a>,
}

/// A parsed container: header and events, plus how many bytes at the end of
/// the event stream could not be read (truncated or corrupted file).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Container<'a> {
    pub header: Header,
    pub events: Vec<Event<'a>>,
    pub unread: usize,
}

/// Reads a little-endian base-128 varint (7 bits per byte, high bit = more),
/// as used for the length of data events. `None` if truncated or longer than
/// 32 bits.
pub fn read_varint(bytes: &[u8], pos: &mut usize) -> Option<usize> {
    let mut value: u64 = 0;
    for shift in (0..35).step_by(7) {
        let b = *bytes.get(*pos)?;
        *pos += 1;
        value |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            return usize::try_from(value).ok().filter(|&v| v <= u32::MAX as usize);
        }
    }
    None
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// Splits an FLP file into its header and events. Never panics: a truncated
/// event stream keeps the events read so far.
pub fn parse_container(bytes: &[u8]) -> Result<Container<'_>, String> {
    if bytes.get(0..4) != Some(b"FLhd") {
        return Err("Not an FL Studio project (missing FLhd header)".into());
    }
    let header_len = u32_at(bytes, 4).ok_or("Truncated FL Studio header")? as usize;
    if header_len < 6 {
        return Err("Corrupted FL Studio header".into());
    }
    let header = Header {
        format: u16_at(bytes, 8).ok_or("Truncated FL Studio header")? as i16,
        channels: u16_at(bytes, 10).ok_or("Truncated FL Studio header")?,
        ppq: u16_at(bytes, 12).ok_or("Truncated FL Studio header")?,
    };
    if header.ppq == 0 {
        return Err("Corrupted FL Studio header (PPQ is 0)".into());
    }
    // Chunks after the header: skip anything that is not FLdt.
    let mut pos = 8usize.saturating_add(header_len);
    let (data, missing) = loop {
        let id = bytes.get(pos..pos + 4).ok_or("No event data in this FL Studio project")?;
        let len = u32_at(bytes, pos + 4).ok_or("No event data in this FL Studio project")? as usize;
        let start = pos + 8;
        if id == b"FLdt" {
            let declared = start.saturating_add(len);
            let end = declared.min(bytes.len());
            break (&bytes[start..end], declared - end);
        }
        pos = start.saturating_add(len);
    };
    let mut events = Vec::new();
    let mut at = 0;
    while at < data.len() {
        let begin = at;
        let id = data[at];
        at += 1;
        let value = match id {
            0..WORD => data.get(at).map(|&b| (Value::Byte(b), 1)),
            WORD..DWORD => u16_at(data, at).map(|w| (Value::Word(w), 2)),
            DWORD..TEXT => u32_at(data, at).map(|d| (Value::Dword(d), 4)),
            _ => read_varint(data, &mut at)
                .and_then(|len| data.get(at..at.checked_add(len)?).map(|d| (Value::Data(d), len))),
        };
        let Some((value, len)) = value else {
            return Ok(Container { header, events, unread: data.len() - begin + missing });
        };
        at += len;
        events.push(Event { id, value });
    }
    Ok(Container { header, events, unread: missing })
}

/// Decodes a text event: UTF-16LE (FL Studio 11.5+) or 8-bit, without the
/// trailing NUL.
fn decode_text(data: &[u8], unicode: bool) -> String {
    let s = if unicode {
        let units: Vec<u16> = data.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&units)
    } else {
        match std::str::from_utf8(data) {
            Ok(s) => s.to_string(),
            // Windows-1252 is the usual 8-bit encoding: Latin-1 is close enough.
            Err(_) => data.iter().map(|&b| b as char).collect(),
        }
    };
    s.trim_end_matches('\0').to_string()
}

// --- Intermediate model ---

#[derive(Clone, Debug, PartialEq)]
struct FlChannel {
    iid: u16,
    kind: u8,
    name: Option<String>,
    internal: String,
    sample: Option<String>,
    insert: Option<i8>,
    color: Option<u32>,
    volume: Option<u32>,
    pan: Option<i32>,
    root: Option<u32>,
    enabled: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct FlNote {
    pos: u32,
    channel: u16,
    len: u32,
    key: u16,
    vel: u8,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct FlPattern {
    name: Option<String>,
    color: Option<u32>,
    length: u32,
    notes: Vec<FlNote>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct FlItem {
    pos: u32,
    index: u16,
    len: u32,
    track: usize,
    /// Raw start offset: ticks (i32) for patterns, -1 when unset.
    start_offset: u32,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct FlArrangement {
    name: Option<String>,
    items: Vec<FlItem>,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct FlSlot {
    internal: String,
    name: Option<String>,
    enabled: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct FlInsert {
    name: Option<String>,
    color: Option<u32>,
    routes: Vec<bool>,
    slots: Vec<FlSlot>,
    volume: Option<i32>,
    pan: Option<i32>,
    /// Route levels by target insert.
    route_levels: BTreeMap<usize, i32>,
    /// Slot enabled flags from the mixer parameters, by slot.
    slot_enabled: BTreeMap<usize, bool>,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct FlProject {
    version: Option<(u32, u32, u32)>,
    ppq: u32,
    tempo: Option<f32>,
    time_sig: (u8, u8),
    title: Option<String>,
    channels: Vec<FlChannel>,
    patterns: BTreeMap<u16, FlPattern>,
    arrangements: Vec<FlArrangement>,
    current_arrangement: Option<usize>,
    inserts: Vec<FlInsert>,
    unread: usize,
}

/// RGBA bytes stored little-endian (PyFLP `ColorEvent`) → `0xRRGGBB`.
fn rgb(v: u32) -> u32 {
    let [r, g, b, _] = v.to_le_bytes();
    (r as u32) << 16 | (g as u32) << 8 | b as u32
}

fn parse_version(s: &str) -> Option<(u32, u32, u32)> {
    let mut it = s.split('.').map(|p| p.trim().parse::<u32>().ok());
    Some((it.next()??, it.next().flatten().unwrap_or(0), it.next().flatten().unwrap_or(0)))
}

fn read_project(bytes: &[u8]) -> Result<FlProject, String> {
    let c = parse_container(bytes)?;
    let mut p = FlProject { ppq: c.header.ppq as u32, time_sig: (4, 4), unread: c.unread, ..Default::default() };
    let mut channel: Option<usize> = None;
    let mut pattern: Option<u16> = None;
    let mut mixer = false;
    let mut insert = 0usize;
    let mut slot = FlSlot { enabled: true, ..Default::default() };
    let mut coarse_tempo: Option<u32> = None;
    let mut fine_tempo = 0u32;

    for e in &c.events {
        let unicode = match p.version {
            Some(v) => v >= (11, 5, 0),
            // No version event: guess from the bytes (ASCII in UTF-16 has zero high bytes).
            None => e.value.data().len() >= 2 && e.value.data()[1] == 0,
        };
        let is_text = (TEXT..DATA).contains(&e.id) || NEW_TEXT_IDS.contains(&e.id);
        let text = || {
            if is_text { decode_text(e.value.data(), unicode) } else { String::new() }
        };
        let v = e.value.int();
        match e.id {
            FL_VERSION => p.version = parse_version(&decode_text(e.value.data(), false)),
            TEMPO => p.tempo = Some(v as f32 / 1000.0),
            TEMPO_COARSE => coarse_tempo = Some(v),
            TEMPO_FINE => fine_tempo = v,
            TIME_SIG_NUM => p.time_sig.0 = v as u8,
            TIME_SIG_BEAT => p.time_sig.1 = v as u8,
            TITLE => p.title = Some(text()).filter(|t| !t.trim().is_empty()),

            CH_NEW => {
                p.channels.push(FlChannel {
                    iid: v as u16,
                    kind: TYPE_NATIVE,
                    name: None,
                    internal: String::new(),
                    sample: None,
                    insert: None,
                    color: None,
                    volume: None,
                    pan: None,
                    root: None,
                    enabled: true,
                });
                channel = Some(p.channels.len() - 1);
                mixer = false;
            }
            PLUGIN_COLOR | PLUGIN_INTERNAL_NAME | PLUGIN_NAME if mixer => match e.id {
                PLUGIN_INTERNAL_NAME => slot.internal = text(),
                PLUGIN_NAME => slot.name = Some(text()),
                _ => {}
            },
            CH_ENABLED | CH_VOL_BYTE | CH_PAN_BYTE | CH_TYPE | CH_ROUTED_TO | CH_VOL_WORD | CH_PAN_WORD
            | CH_ROOT_NOTE | CH_NAME_OLD | CH_SAMPLE_PATH | CH_LEVELS | PLUGIN_COLOR | PLUGIN_INTERNAL_NAME
            | PLUGIN_NAME => {
                let Some(ch) = channel.and_then(|i| p.channels.get_mut(i)) else {
                    continue;
                };
                match e.id {
                    CH_ENABLED => ch.enabled = v != 0,
                    CH_VOL_BYTE => ch.volume = ch.volume.or(Some(v * 100)),
                    CH_PAN_BYTE => ch.pan = ch.pan.or(Some(v as i32 * 100)),
                    CH_VOL_WORD => ch.volume = Some(v),
                    CH_PAN_WORD => ch.pan = Some(v as i32),
                    CH_TYPE => ch.kind = v as u8,
                    CH_ROUTED_TO => ch.insert = Some(v as u8 as i8),
                    CH_ROOT_NOTE => ch.root = Some(v),
                    CH_NAME_OLD => ch.name = ch.name.take().or(Some(text())),
                    PLUGIN_NAME => ch.name = Some(text()),
                    CH_SAMPLE_PATH => ch.sample = Some(text()).filter(|s| !s.is_empty()),
                    PLUGIN_INTERNAL_NAME => ch.internal = text(),
                    PLUGIN_COLOR => ch.color = Some(rgb(v)),
                    CH_LEVELS => {
                        // PyFLP `LevelsEvent`: pan i32, volume u32, ...
                        let d = e.value.data();
                        if let Some(pan) = u32_at(d, 0) {
                            ch.pan = Some(pan as i32);
                        }
                        if let Some(vol) = u32_at(d, 4) {
                            ch.volume = Some(vol);
                        }
                    }
                    _ => {}
                }
            }

            PAT_NEW => {
                pattern = Some(v as u16);
                p.patterns.entry(v as u16).or_default();
            }
            PAT_NAME | PAT_COLOR | PAT_LENGTH | PAT_NOTES => {
                let Some(pat) = pattern.and_then(|i| p.patterns.get_mut(&i)) else {
                    continue;
                };
                match e.id {
                    PAT_NAME => pat.name = Some(text()).filter(|t| !t.trim().is_empty()),
                    PAT_COLOR => pat.color = Some(rgb(v)),
                    PAT_LENGTH => pat.length = v,
                    _ => {
                        // PyFLP `NotesEvent`: 24 bytes per note.
                        for n in e.value.data().chunks_exact(24) {
                            pat.notes.push(FlNote {
                                pos: u32_at(n, 0).unwrap_or(0),
                                channel: u16_at(n, 6).unwrap_or(0),
                                len: u32_at(n, 8).unwrap_or(0),
                                key: u16_at(n, 12).unwrap_or(60),
                                vel: n[21],
                            });
                        }
                    }
                }
            }

            ARR_NEW => p.arrangements.push(FlArrangement::default()),
            ARR_CURRENT => p.current_arrangement = Some(v as usize),
            ARR_NAME => {
                if let Some(a) = p.arrangements.last_mut() {
                    a.name = Some(text());
                }
            }
            ARR_PLAYLIST => {
                if p.arrangements.is_empty() {
                    p.arrangements.push(FlArrangement::default());
                }
                let new_tracks = p.version.is_none_or(|v| v >= (12, 9, 1));
                let max_track: u16 = if new_tracks { 499 } else { 198 };
                let d = e.value.data();
                // PyFLP `PlaylistEvent`: 32 bytes per item, 60 from FL Studio 21.
                let size = match p.version {
                    Some(v) if v >= (21, 0, 0) => 60,
                    Some(_) => 32,
                    None if d.len() % 60 == 0 => 60,
                    None => 32,
                };
                let items = d.chunks_exact(size).filter_map(|it| {
                    Some(FlItem {
                        pos: u32_at(it, 0)?,
                        index: u16_at(it, 6)?,
                        len: u32_at(it, 8)?,
                        track: max_track.checked_sub(u16_at(it, 12)?)? as usize,
                        start_offset: u32_at(it, 24)?,
                    })
                });
                let arr = p.arrangements.last_mut().expect("pushed above");
                arr.items.extend(items);
            }
            TRACK_DATA => {}

            INS_NAME | INS_COLOR | INS_FLAGS | INS_ROUTING | SLOT_INDEX => {
                mixer = true;
                channel = None;
                if p.inserts.len() <= insert {
                    p.inserts.resize(insert + 1, FlInsert::default());
                }
                let ins = &mut p.inserts[insert];
                match e.id {
                    INS_NAME => ins.name = Some(text()).filter(|t| !t.trim().is_empty()),
                    INS_COLOR => ins.color = Some(rgb(v)),
                    INS_ROUTING => ins.routes = e.value.data().iter().map(|&b| b != 0).collect(),
                    SLOT_INDEX => {
                        ins.slots.push(std::mem::replace(&mut slot, FlSlot { enabled: true, ..Default::default() }))
                    }
                    _ => {}
                }
            }
            INS_OUTPUT if mixer => insert += 1,
            MIXER_PARAMS => {
                // PyFLP `MixerParamsEvent`: 12 bytes per parameter.
                for it in e.value.data().chunks_exact(12) {
                    let id = it[4];
                    let target = u16_at(it, 6).unwrap_or(0);
                    let msg = u32_at(it, 8).unwrap_or(0) as i32;
                    let ins_idx = ((target >> 6) & 0x7f) as usize;
                    let slot_idx = (target & 0x3f) as usize;
                    if p.inserts.len() <= ins_idx {
                        p.inserts.resize(ins_idx + 1, FlInsert::default());
                    }
                    let ins = &mut p.inserts[ins_idx];
                    match id {
                        PARAM_SLOT_ENABLED => {
                            ins.slot_enabled.insert(slot_idx, msg != 0);
                        }
                        PARAM_VOLUME => ins.volume = Some(msg),
                        PARAM_PAN => ins.pan = Some(msg),
                        PARAM_ROUTE_START..PARAM_VOLUME => {
                            ins.route_levels.insert((id - PARAM_ROUTE_START) as usize, msg);
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    if p.tempo.is_none() {
        p.tempo = coarse_tempo.map(|c| c as f32 + fine_tempo as f32 / 1000.0);
    }
    Ok(p)
}

// --- Import report ---

/// What an import kept, approximated and left out, to show to the user.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ImportReport {
    pub mapped: Vec<String>,
    pub approximated: Vec<String>,
    pub skipped: Vec<String>,
}

impl ImportReport {
    /// One line for a toast.
    pub fn summary(&self) -> String {
        tr_fmt!(
            "FL Studio import: {} mapped, {} approximated, {} skipped",
            self.mapped.len(),
            self.approximated.len(),
            self.skipped.len()
        )
    }

    /// Every entry, prefixed by its category.
    pub fn lines(&self) -> Vec<String> {
        let tag = |t: &'static str| move |s: &String| tr_fmt!("{}: {}", t, s);
        self.mapped
            .iter()
            .map(tag(tr("Mapped")))
            .chain(self.approximated.iter().map(tag(tr("Approximated"))))
            .chain(self.skipped.iter().map(tag(tr("Skipped"))))
            .collect()
    }

    /// [`ImportReport::lines`] wrapped at `width` characters for narrow
    /// lists: (text, true when the row continues the previous entry).
    pub fn rows(&self, width: usize) -> Vec<(String, bool)> {
        let mut rows = Vec::new();
        for line in self.lines() {
            let mut row = String::new();
            let mut more = false;
            for word in line.split(' ') {
                if !row.is_empty() && row.chars().count() + 1 + word.chars().count() > width {
                    rows.push((std::mem::take(&mut row), more));
                    more = true;
                }
                if !row.is_empty() {
                    row.push(' ');
                }
                row.push_str(word);
            }
            rows.push((row, more));
        }
        rows
    }
}

// --- Device mapping ---

/// Closest native instrument for an FL Studio generator (internal name, or
/// plugin name for wrapped VSTs). Settings are never converted.
fn instrument_for(name: &str) -> &'static str {
    let n = name.to_lowercase();
    let has = |keys: &[&str]| keys.iter().any(|k| n.contains(k));
    if has(&["boobass", "transistor", "ts404", "303"]) {
        "bass303"
    } else if has(&["kick"]) {
        "kick"
    } else if has(&["fpc", "battery", "drum machine"]) {
        "drum_machine"
    } else if has(&["drumsynth", "drum synth", "drumaxx"]) {
        "drumsynth"
    } else if has(&["slicer", "slicex"]) {
        "slicer"
    } else if has(&["granul"]) {
        "granular"
    } else if has(&["pluck", "sakura"]) {
        "pluck"
    } else if has(&["fl keys", "piano", "rhodes", "keys"]) {
        "epiano"
    } else if has(&["dx10", "sytrus", "fm8", " fm"]) {
        "fm"
    } else if has(&[
        "harmor",
        "harmless",
        "serum",
        "vital",
        "wavetable",
        "morphine",
        "poizone",
        "toxic",
        "sawer",
        "flex",
    ]) {
        "wavetable"
    } else if has(&["midi out"]) {
        "midi_out"
    } else if has(&["directwave", "sampler"]) {
        "sampler"
    } else {
        "pulse"
    }
}

/// Native effect for an FL Studio effect, with `true` when it is a direct
/// equivalent (rather than the closest match). `None`: nothing comparable.
fn effect_for(name: &str) -> Option<(&'static str, bool)> {
    let n = name.to_lowercase();
    let exact = [
        ("fruity parametric eq 2", "eq"),
        ("fruity parametric eq", "eq"),
        ("fruity limiter", "limiter"),
        ("fruity compressor", "compressor"),
        ("fruity reeverb 2", "reverb"),
        ("fruity reeverb", "reverb"),
        ("fruity delay 3", "delay"),
        ("fruity delay 2", "delay"),
        ("fruity delay", "delay"),
        ("fruity chorus", "chorus"),
        ("fruity phaser", "phaser"),
        ("fruity filter", "autofilter"),
        ("fruity balance", "utility"),
        ("fruity stereo enhancer", "imager"),
        ("fruity multiband compressor", "ott"),
        ("fruity soft clipper", "saturator"),
        ("fruity fast dist", "saturator"),
        ("fruity blood overdrive", "saturator"),
        ("fruity squeeze", "bitcrusher"),
        ("fruity bass boost", "eq"),
        ("fruity 7 band eq", "eq"),
        ("fruity convolver", "reverb"),
        ("fruity stereo shaper", "imager"),
        ("fruity flangus", "chorus"),
        ("fruity flanger", "chorus"),
        ("soundgoodizer", "ott"),
        ("maximus", "ott"),
        ("gross beat", "pitch"),
    ];
    if let Some((fl, ours)) = exact.iter().find(|(fl, _)| n == *fl) {
        let direct =
            matches!(*fl, "fruity parametric eq 2" | "fruity parametric eq" | "fruity limiter" | "fruity compressor")
                || fl.starts_with("fruity reeverb")
                || fl.starts_with("fruity delay")
                || matches!(
                    *fl,
                    "fruity chorus" | "fruity phaser" | "fruity filter" | "fruity balance" | "fruity stereo enhancer"
                );
        return Some((ours, direct));
    }
    let has = |keys: &[&str]| keys.iter().any(|k| n.contains(k));
    let kind = if has(&["pro-q", " eq", "equaliz"]) || n.starts_with("eq") {
        "eq"
    } else if has(&["pro-l", "limiter", "maximizer"]) {
        "limiter"
    } else if has(&["pro-c", "compressor", "comp "]) {
        "compressor"
    } else if has(&["ott", "multiband"]) {
        "ott"
    } else if has(&["reverb", "valhalla", "pro-r", "verb"]) {
        "reverb"
    } else if has(&["delay", "echo"]) {
        "delay"
    } else if has(&["saturat", "decapitator", "saturn", "distort", "overdrive", "clipper"]) {
        "saturator"
    } else if has(&["chorus"]) {
        "chorus"
    } else if has(&["phaser"]) {
        "phaser"
    } else if has(&["crush"]) {
        "bitcrusher"
    } else {
        return None;
    };
    Some((kind, false))
}

fn effect_name(kind: &str) -> &'static str {
    ponzik_engine::registry::effect_kind(kind).map_or("?", |k| k.name)
}

fn instrument_name(kind: &str) -> &'static str {
    ponzik_engine::registry::instrument_kind(kind).map_or("?", |k| k.name)
}

// --- Sample lookup ---

/// Finds a sample referenced by an FL Studio project on this machine: the path
/// as written (Windows separators converted), then the file name next to the
/// project and in its sub-folders (two levels, as left by "zipped" projects
/// and collected sample folders).
pub fn resolve_sample(raw: &str, project_dir: Option<&Path>) -> Option<String> {
    let unix = raw.replace('\\', "/");
    if !unix.contains('%') && Path::new(&unix).is_file() {
        return Some(unix);
    }
    let file = unix.rsplit('/').next().filter(|f| !f.is_empty())?;
    let dir = project_dir?;
    fn find(dir: &Path, file: &str, depth: usize) -> Option<std::path::PathBuf> {
        let direct = dir.join(file);
        if direct.is_file() {
            return Some(direct);
        }
        if depth == 0 {
            return None;
        }
        let entries = std::fs::read_dir(dir).ok()?;
        entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()).find_map(|p| find(&p, file, depth - 1))
    }
    find(dir, file, 2).map(|p| p.to_string_lossy().into_owned())
}

// --- Conversion ---

/// Converts an FL Studio project into a new Ponzik project. `resolve` turns a
/// sample path stored in the file into a readable WAV path (see
/// [`resolve_sample`]); `name` is used when the project has no title.
pub fn import_flp(
    bytes: &[u8],
    name: &str,
    resolve: &dyn Fn(&str) -> Option<String>,
) -> Result<(Project, ImportReport), String> {
    let fl = read_project(bytes)?;
    let mut report = ImportReport::default();
    let mut p = Project::empty();
    p.name = fl.title.clone().unwrap_or_else(|| name.to_string());
    let ppq = fl.ppq.max(1) as u64;
    let tick = |v: u32| -> u32 { (v as u64 * PPQ as u64 / ppq).min(u32::MAX as u64) as u32 };

    if let Some((a, b, c)) = fl.version {
        report.mapped.push(tr_fmt!("FL Studio {}.{}.{} project, {} PPQ converted to {}", a, b, c, fl.ppq, PPQ));
    }
    if fl.unread > 0 {
        report
            .skipped
            .push(tr_fmt!("The file is truncated or corrupted: {} bytes of project data could not be read", fl.unread));
    }
    match fl.tempo {
        Some(t) if t.is_finite() && t > 0.0 => {
            p.bpm = t.clamp(10.0, 999.0);
            report.mapped.push(tr_fmt!("Tempo {:.2} BPM", p.bpm));
        }
        _ => report.approximated.push(tr("No tempo found: 120 BPM").into()),
    }
    if fl.time_sig != (4, 4) && fl.time_sig.0 > 0 {
        report.approximated.push(tr_fmt!(
            "Time signature {}/{}: Ponzik shows 4/4 bars, positions are unchanged",
            fl.time_sig.0, fl.time_sig.1
        ));
    }

    // Channels used by playlist items (audio clips and automation clips).
    let arrangement = fl
        .current_arrangement
        .and_then(|i| fl.arrangements.get(i))
        .filter(|a| !a.items.is_empty())
        .or_else(|| fl.arrangements.iter().find(|a| !a.items.is_empty()));
    let items: &[FlItem] = arrangement.map_or(&[], |a| &a.items);
    let in_playlist = |iid: u16| items.iter().any(|it| it.index <= PATTERN_BASE && it.index == iid);
    let in_patterns = |iid: u16| fl.patterns.values().any(|pat| pat.notes.iter().any(|n| n.channel == iid));

    let track_of = |insert: Option<i8>, what: &str, report: &mut ImportReport| -> usize {
        match insert {
            Some(i) if i > 0 && (i as usize) < TRACKS => i as usize,
            Some(i) if i as usize >= TRACKS => {
                report.approximated.push(tr_fmt!(
                    "{}: FL insert {} does not exist in Ponzik ({} tracks), routed to Master",
                    what,
                    i,
                    TRACKS - 1
                ));
                MASTER
            }
            _ => MASTER,
        }
    };

    // FL channel iid → Ponzik channel index.
    let mut channel_map: BTreeMap<u16, usize> = BTreeMap::new();
    // FL channel iid → (WAV path, mixer track) for audio clips.
    let mut audio_map: BTreeMap<u16, (String, usize)> = BTreeMap::new();
    let mut skipped_channels: BTreeMap<u16, String> = BTreeMap::new();
    let mut generators = 0;
    let mut empty_samplers = Vec::new();
    for ch in &fl.channels {
        let label = ch.name.clone().filter(|n| !n.trim().is_empty()).unwrap_or_else(|| {
            if ch.internal.is_empty() { tr_fmt!("Channel {}", ch.iid + 1) } else { ch.internal.clone() }
        });
        match ch.kind {
            TYPE_AUTOMATION => {
                report.skipped.push(tr_fmt!("Automation clip '{}': automation is not imported yet", label));
                skipped_channels.insert(ch.iid, label);
                continue;
            }
            TYPE_LAYER => {
                report
                    .skipped
                    .push(tr_fmt!("Layer channel '{}': layers are not supported, its notes are dropped", label));
                skipped_channels.insert(ch.iid, label);
                continue;
            }
            _ => {}
        }
        let sample_like =
            ch.kind == TYPE_SAMPLER || (ch.internal.is_empty() && matches!(ch.kind, TYPE_NATIVE | TYPE_INSTRUMENT));
        let wav = ch.sample.as_deref().and_then(|raw| {
            if !raw.to_lowercase().ends_with(".wav") {
                report.skipped.push(tr_fmt!("'{}': sample {} is not a WAV file (only WAV is supported)", label, raw));
                return None;
            }
            let found = resolve(raw);
            if found.is_none() {
                report.skipped.push(tr_fmt!("'{}': sample not found on this computer: {}", label, raw));
            }
            found
        });
        let track = track_of(ch.insert, &tr_fmt!("Channel '{}'", label), &mut report);
        if sample_like && in_playlist(ch.iid) {
            match &wav {
                Some(path) => {
                    audio_map.insert(ch.iid, (path.clone(), track));
                }
                None => {
                    skipped_channels.insert(ch.iid, label.clone());
                }
            }
            if !in_patterns(ch.iid) {
                continue;
            }
        }
        let instrument = if sample_like {
            let mut def = InstrumentDef::new("sampler");
            def.sample = wav;
            if def.sample.is_none() && ch.sample.is_none() {
                empty_samplers.push(label.clone());
            }
            def
        } else {
            let source = if ch.internal.eq_ignore_ascii_case("fruity wrapper") {
                tr_fmt!("VST plugin '{}'", ch.name.as_deref().unwrap_or("?"))
            } else {
                tr_fmt!("generator '{}'", ch.internal)
            };
            let kind =
                instrument_for(if ch.internal.eq_ignore_ascii_case("fruity wrapper") { &label } else { &ch.internal });
            report
                .approximated
                .push(tr_fmt!("'{}': FL {} replaced by {} (settings not imported)", label, source, tr(instrument_name(kind))));
            generators += 1;
            InstrumentDef::new(kind)
        };
        channel_map.insert(ch.iid, p.channels.len());
        p.channels.push(ChannelDef {
            name: label,
            instrument,
            note_fx: Vec::new(),
            gain: (ch.volume.unwrap_or(10_000) as f32 / FL_UNITY).clamp(0.0, 1.25),
            pan: ((ch.pan.unwrap_or(6400) - 6400) as f32 / 6400.0).clamp(-1.0, 1.0),
            muted: !ch.enabled,
            track,
            root_note: ch.root.unwrap_or(60).min(127) as u8,
            color: ch.color.unwrap_or(0),
            modulators: Vec::new(),
        });
    }
    if !p.channels.is_empty() {
        let samplers = p.channels.len() - generators;
        report.mapped.push(tr_fmt!(
            "{} channels ({} samplers, {} instruments): name, colour, volume, pan, mixer track",
            p.channels.len(),
            samplers,
            generators
        ));
    }
    if !empty_samplers.is_empty() {
        report.approximated.push(tr_fmt!(
            "{} Sampler channels without a sample: {}",
            empty_samplers.len(),
            empty_samplers.join(", ")
        ));
    }
    if !audio_map.is_empty() {
        report.mapped.push(tr_fmt!("{} audio clip sources", audio_map.len()));
    }

    // Patterns.
    let mut pattern_map: BTreeMap<u16, usize> = BTreeMap::new();
    let mut patterns = Vec::new();
    let (mut kept, mut dropped, mut steps) = (0usize, 0usize, 0usize);
    for (&iid, pat) in &fl.patterns {
        let mut notes = vec![Vec::new(); p.channels.len()];
        let mut end = 0u32;
        for n in &pat.notes {
            let Some(&ch) = channel_map.get(&n.channel) else {
                dropped += 1;
                continue;
            };
            let start = tick(n.pos);
            if start >= MAX_TICK {
                dropped += 1;
                continue;
            }
            // Length 0: a step from the step sequencer.
            let len = if n.len == 0 {
                steps += 1;
                TICKS_PER_STEP
            } else {
                tick(n.len).clamp(1, MAX_TICK)
            };
            end = end.max(start.saturating_add(len));
            notes[ch].push(Note { start, len, key: n.key.min(127) as u8, vel: (n.vel as f32 / 128.0).clamp(0.0, 1.0) });
            kept += 1;
        }
        let looped = tick(pat.length).min(MAX_TICK);
        let len = looped.max(end).div_ceil(TICKS_PER_BAR).max(1) * TICKS_PER_BAR;
        for v in &mut notes {
            v.sort_by_key(|n: &Note| (n.start, n.key));
        }
        // Empty, unnamed patterns are FL Studio's placeholders: skip them.
        if pat.name.is_none()
            && pat.notes.is_empty()
            && !items.iter().any(|it| it.index > PATTERN_BASE && it.index - PATTERN_BASE == iid)
        {
            continue;
        }
        pattern_map.insert(iid, patterns.len());
        patterns.push(PatternDef {
            name: pat.name.clone().unwrap_or_else(|| format!("Pattern {iid}")),
            len,
            notes,
            color: pat.color.unwrap_or(0),
        });
    }
    if !patterns.is_empty() {
        report.mapped.push(tr_fmt!("{} patterns, {} notes ({} step-sequencer steps)", patterns.len(), kept, steps));
        p.patterns = patterns;
    }
    if dropped > 0 {
        report.skipped.push(tr_fmt!("{} notes on channels that could not be imported", dropped));
    }

    // Playlist.
    if let Some(arr) = arrangement {
        let others = fl.arrangements.iter().filter(|a| !a.items.is_empty()).count().saturating_sub(1);
        if others > 0 {
            report.skipped.push(tr_fmt!(
                "{} other arrangement(s): only '{}' is imported",
                others,
                arr.name.as_deref().unwrap_or(tr("the current arrangement"))
            ));
        }
        // Playlist tracks map to lanes; spread-out projects are compacted.
        let mut used: Vec<usize> = arr.items.iter().map(|i| i.track).collect();
        used.sort_unstable();
        used.dedup();
        let compact = used.last().is_some_and(|&t| t >= 64);
        let lane = |t: usize| {
            if compact { used.binary_search(&t).unwrap_or(0) } else { t }
        };
        if compact {
            report.approximated.push(tr("Playlist tracks beyond 64 were packed together").into());
        }
        let (mut automation_items, mut missing_items, mut far_items, mut sampleless_items) = (0, 0, 0, 0);
        for it in &arr.items {
            let start = tick(it.pos);
            if start >= MAX_TICK {
                far_items += 1;
                continue;
            }
            if it.index > PATTERN_BASE {
                let Some(&pattern) = pattern_map.get(&(it.index - PATTERN_BASE)) else {
                    missing_items += 1;
                    continue;
                };
                let offset = if (it.start_offset as i32) > 0 { tick(it.start_offset) } else { 0 };
                let len = if it.len == 0 { p.patterns[pattern].len } else { tick(it.len).clamp(1, MAX_TICK) };
                p.clips.push(ClipDef { pattern, lane: lane(it.track), start, len, offset });
            } else if let Some((path, track)) = audio_map.get(&it.index) {
                p.audio_clips.push(AudioClipDef {
                    path: path.clone(),
                    lane: lane(it.track),
                    track: *track,
                    start,
                    len: tick(it.len).clamp(1, MAX_TICK),
                    offset: 0.0,
                    gain: 1.0,
                    fade_in: 0.0,
                    fade_out: 0.0,
                    ..Default::default()
                });
            } else if fl.channels.iter().any(|c| c.iid == it.index && c.kind == TYPE_AUTOMATION) {
                automation_items += 1;
            } else if fl.channels.iter().any(|c| c.iid == it.index) {
                sampleless_items += 1;
            } else {
                missing_items += 1;
            }
        }
        if !p.clips.is_empty() {
            p.mode = PlayMode::Song;
            report.mapped.push(tr_fmt!("{} pattern clips in the playlist", p.clips.len()));
        }
        if !p.audio_clips.is_empty() {
            report.approximated.push(tr_fmt!(
                "{} audio clips: placed whole from their start (trims, stretching and fades are not imported)",
                p.audio_clips.len()
            ));
        }
        if automation_items > 0 {
            report.skipped.push(tr_fmt!("{} automation clips in the playlist", automation_items));
        }
        if sampleless_items > 0 {
            report.skipped.push(tr_fmt!("{} audio clips whose sample is missing or unsupported", sampleless_items));
        }
        if missing_items > 0 {
            report.skipped.push(tr_fmt!("{} playlist clips pointing to a missing pattern or channel", missing_items));
        }
        if far_items > 0 {
            report.skipped.push(tr_fmt!("{} playlist clips placed too far in the song", far_items));
        }
    }

    // Mixer.
    let mut used_inserts = 0;
    let (mut fx_direct, mut sends) = (0, 0);
    for (i, ins) in fl.inserts.iter().enumerate() {
        let label = ins.name.clone().unwrap_or_else(|| if i == 0 { "Master".into() } else { format!("Insert {i}") });
        let routed = p.channels.iter().any(|c| c.track == i) || p.audio_clips.iter().any(|c| c.track == i);
        let has_fx = ins.slots.iter().any(|s| !s.internal.is_empty());
        if i >= TRACKS {
            if ins.name.is_some() || has_fx {
                report.skipped.push(tr_fmt!("Mixer insert {} '{}': Ponzik has {} tracks", i, label, TRACKS - 1));
            }
            continue;
        }
        let interesting = i == MASTER || routed || has_fx || ins.name.is_some();
        let t = &mut p.tracks[i];
        if interesting && i != MASTER {
            t.name = label.clone();
            used_inserts += 1;
        }
        if let Some(v) = ins.volume {
            t.gain = (v as f32 / FL_UNITY).clamp(0.0, 1.25);
        }
        if let Some(pan) = ins.pan {
            t.pan = (pan as f32 / 6400.0).clamp(-1.0, 1.0);
        }
        if let Some(c) = ins.color.filter(|_| interesting && i != MASTER) {
            t.color = c;
        }
        // Routing: FL can route to any number of inserts. The master (or the
        // first target) becomes the output, the others sends.
        if i != MASTER {
            let mut targets: Vec<(usize, f32)> = ins
                .routes
                .iter()
                .enumerate()
                .filter(|&(to, &on)| on && to != i)
                .filter_map(|(to, _)| {
                    let level = ins.route_levels.get(&to).map_or(1.0, |&l| l as f32 / FL_UNITY);
                    (to == MASTER || level > 0.0).then_some((to, level.clamp(0.0, 1.25)))
                })
                .collect();
            if targets.iter().any(|&(to, _)| to >= TRACKS) {
                report.skipped.push(tr_fmt!("'{}': routes to inserts above {} dropped", label, TRACKS - 1));
                targets.retain(|&(to, _)| to < TRACKS);
            }
            if !ins.routes.is_empty() && targets.is_empty() {
                if interesting {
                    report.approximated.push(tr_fmt!("'{}' is not routed anywhere in FL Studio: muted", label));
                }
                t.muted = true;
            }
            let output = if targets.iter().any(|&(to, _)| to == MASTER) {
                MASTER
            } else {
                targets.first().map_or(MASTER, |&(to, _)| to)
            };
            t.output = output;
            let extra: Vec<(usize, f32)> = targets.into_iter().filter(|&(to, _)| to != output).collect();
            if extra.len() > SEND_SLOTS {
                report
                    .skipped
                    .push(tr_fmt!("'{}': {} routes beyond {} sends dropped", label, extra.len() - SEND_SLOTS, SEND_SLOTS));
            }
            for (slot, &(to, level)) in t.sends.iter_mut().zip(extra.iter()) {
                *slot = (to, level);
                sends += 1;
            }
        }
        // Effect slots.
        let mut slot_out = 0;
        for (s, fx) in ins.slots.iter().enumerate() {
            if fx.internal.is_empty() {
                continue;
            }
            let shown = if fx.internal.eq_ignore_ascii_case("fruity wrapper") {
                fx.name.clone().unwrap_or_else(|| tr("VST plugin").into())
            } else {
                fx.internal.clone()
            };
            let Some((kind, direct)) = effect_for(&shown) else {
                report.skipped.push(tr_fmt!("'{}' slot {}: {} has no Ponzik equivalent", label, s + 1, shown));
                continue;
            };
            if slot_out >= INSERT_SLOTS {
                report.skipped.push(tr_fmt!("'{}' slot {}: {} (no free insert slot left)", label, s + 1, shown));
                continue;
            }
            let mut def = EffectDef::new(kind);
            def.bypass = !fx.enabled || ins.slot_enabled.get(&s) == Some(&false);
            t.inserts[slot_out] = Some(def);
            slot_out += 1;
            let line = tr_fmt!("'{}' slot {}: {} → {} (default settings)", label, s + 1, shown, tr(effect_name(kind)));
            if direct {
                fx_direct += 1;
                report.mapped.push(line);
            } else {
                report.approximated.push(line);
            }
        }
    }
    if !fl.inserts.is_empty() {
        report
            .mapped
            .push(tr_fmt!("Mixer: master and {} inserts (name, colour, volume, pan), {} sends", used_inserts, sends));
    }
    if p.routing_order().is_none() {
        for t in p.tracks.iter_mut().skip(1) {
            t.output = MASTER;
            t.sends = [(MASTER, 0.0); SEND_SLOTS];
        }
        report.approximated.push(tr("The mixer routing loops in Ponzik: every track now goes to the Master").into());
    }
    if fx_direct > 0 || !p.channels.is_empty() {
        report.approximated.push(
            tr("Plugin settings, envelopes and channel effects are not converted: devices start from their defaults")
                .into(),
        );
    }
    Ok((p, report))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds an FLP event stream.
    #[derive(Default)]
    struct W(Vec<u8>);

    impl W {
        fn byte(&mut self, id: u8, v: u8) -> &mut Self {
            assert!(id < WORD);
            self.0.extend_from_slice(&[id, v]);
            self
        }
        fn word(&mut self, id: u8, v: u16) -> &mut Self {
            assert!((WORD..DWORD).contains(&id));
            self.0.push(id);
            self.0.extend_from_slice(&v.to_le_bytes());
            self
        }
        fn dword(&mut self, id: u8, v: u32) -> &mut Self {
            assert!((DWORD..TEXT).contains(&id));
            self.0.push(id);
            self.0.extend_from_slice(&v.to_le_bytes());
            self
        }
        fn data(&mut self, id: u8, d: &[u8]) -> &mut Self {
            assert!(id >= TEXT);
            self.0.push(id);
            let mut n = d.len();
            loop {
                let b = (n & 0x7f) as u8;
                n >>= 7;
                self.0.push(if n > 0 { b | 0x80 } else { b });
                if n == 0 {
                    break;
                }
            }
            self.0.extend_from_slice(d);
            self
        }
        fn ascii(&mut self, id: u8, s: &str) -> &mut Self {
            let mut d = s.as_bytes().to_vec();
            d.push(0);
            self.data(id, &d)
        }
        fn text(&mut self, id: u8, s: &str) -> &mut Self {
            let mut d: Vec<u8> = s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
            d.extend_from_slice(&[0, 0]);
            self.data(id, &d)
        }
        fn file(&self, ppq: u16) -> Vec<u8> {
            let mut f = b"FLhd".to_vec();
            f.extend_from_slice(&6u32.to_le_bytes());
            f.extend_from_slice(&0i16.to_le_bytes());
            f.extend_from_slice(&2u16.to_le_bytes());
            f.extend_from_slice(&ppq.to_le_bytes());
            f.extend_from_slice(b"FLdt");
            f.extend_from_slice(&(self.0.len() as u32).to_le_bytes());
            f.extend_from_slice(&self.0);
            f
        }
    }

    fn note(pos: u32, channel: u16, len: u32, key: u16, vel: u8) -> Vec<u8> {
        let mut n = Vec::new();
        n.extend_from_slice(&pos.to_le_bytes());
        n.extend_from_slice(&0x4000u16.to_le_bytes());
        n.extend_from_slice(&channel.to_le_bytes());
        n.extend_from_slice(&len.to_le_bytes());
        n.extend_from_slice(&key.to_le_bytes());
        n.extend_from_slice(&[0, 0, 120, 0, 64, 0, 64, vel, 128, 128]);
        assert_eq!(n.len(), 24);
        n
    }

    fn item(pos: u32, index: u16, len: u32, track: u16) -> Vec<u8> {
        let mut it = Vec::new();
        it.extend_from_slice(&pos.to_le_bytes());
        it.extend_from_slice(&PATTERN_BASE.to_le_bytes());
        it.extend_from_slice(&index.to_le_bytes());
        it.extend_from_slice(&len.to_le_bytes());
        it.extend_from_slice(&(499 - track).to_le_bytes());
        it.extend_from_slice(&0u16.to_le_bytes());
        it.extend_from_slice(&[120, 0, 64, 0, 64, 100, 128, 128]);
        it.extend_from_slice(&u32::MAX.to_le_bytes());
        it.extend_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(it.len(), 32);
        it
    }

    fn param(id: u8, insert: u16, slot: u16, msg: i32) -> Vec<u8> {
        let mut p = vec![0, 0, 0, 0, id, 0];
        p.extend_from_slice(&((insert << 6) | slot).to_le_bytes());
        p.extend_from_slice(&msg.to_le_bytes());
        p
    }

    /// An FL 20 project at 192 PPQ: a sampler, a 3x Osc, two patterns, a playlist and a mixer.
    fn sample_project() -> Vec<u8> {
        let mut w = W::default();
        w.ascii(FL_VERSION, "20.8.4.2576").dword(TEMPO, 140_000).byte(TIME_SIG_NUM, 4).byte(TIME_SIG_BEAT, 4);
        w.text(TITLE, "Night drive");
        // Pattern notes come before the channels in FL 20 files.
        let mut notes = note(0, 0, 0, 60, 100);
        notes.extend(note(192, 0, 0, 60, 128));
        notes.extend(note(384, 1, 384, 64, 64));
        notes.extend(note(0, 7, 96, 60, 100)); // channel that does not exist
        w.word(PAT_NEW, 1).data(PAT_NOTES, &notes);
        // Channel 0: sampler with a kick.
        w.word(CH_NEW, 0).byte(CH_TYPE, TYPE_SAMPLER).text(PLUGIN_INTERNAL_NAME, "").text(PLUGIN_NAME, "Kick");
        w.dword(PLUGIN_COLOR, 0x0030_20ff);
        let mut levels = 3200u32.to_le_bytes().to_vec();
        levels.extend(12_800u32.to_le_bytes());
        w.data(CH_LEVELS, &levels).byte(CH_ROUTED_TO, 3).text(CH_SAMPLE_PATH, "C:\\Samples\\Kick 01.wav");
        // Channel 1: 3x Osc.
        w.word(CH_NEW, 1).byte(CH_TYPE, TYPE_NATIVE).text(PLUGIN_INTERNAL_NAME, "3x Osc").text(PLUGIN_NAME, "Lead");
        w.byte(CH_ROUTED_TO, 4);
        // Channel 2: automation clip.
        w.word(CH_NEW, 2).byte(CH_TYPE, TYPE_AUTOMATION).text(PLUGIN_NAME, "Cutoff");
        // Pattern names.
        w.word(PAT_NEW, 1).text(PAT_NAME, "Intro").dword(PAT_COLOR, 0x0000_ff00);
        w.word(PAT_NEW, 2).text(PAT_NAME, "Empty");
        // Playlist.
        w.word(ARR_NEW, 0).text(ARR_NAME, "Arrangement");
        let mut pl = item(768, PATTERN_BASE + 1, 768, 2);
        pl.extend(item(0, PATTERN_BASE + 1, 768, 0));
        pl.extend(item(0, 2, 768, 5)); // automation clip
        w.data(ARR_PLAYLIST, &pl);
        w.word(ARR_CURRENT, 0);
        // Mixer: master, then inserts 1..=4.
        for i in 0..5u16 {
            if i == 3 {
                w.text(INS_NAME, "Drums");
            }
            w.data(INS_FLAGS, &[0; 12]);
            if i == 3 {
                w.text(PLUGIN_INTERNAL_NAME, "Fruity Limiter").word(SLOT_INDEX, 0);
                w.text(PLUGIN_INTERNAL_NAME, "Fruity NoteBook 2").word(SLOT_INDEX, 1);
                w.text(PLUGIN_INTERNAL_NAME, "Fruity Wrapper").text(PLUGIN_NAME, "ValhallaRoom").word(SLOT_INDEX, 2);
            }
            let mut routes = vec![0u8; 127];
            if i == 3 {
                routes[1] = 1; // Drums → insert 1 (a bus) and master
                routes[0] = 1;
            } else if i > 0 {
                routes[0] = 1;
            }
            w.data(INS_ROUTING, &routes).dword(INS_OUTPUT, u32::MAX);
        }
        let mut params = param(PARAM_VOLUME, 3, 0, 6400);
        params.extend(param(PARAM_PAN, 3, 0, -3200));
        params.extend(param(PARAM_ROUTE_START + 1, 3, 0, 9600));
        params.extend(param(PARAM_SLOT_ENABLED, 3, 0, 0));
        w.data(MIXER_PARAMS, &params);
        w.file(192)
    }

    #[test]
    fn varints() {
        let mut pos = 0;
        assert_eq!(read_varint(&[5], &mut pos), Some(5));
        assert_eq!(pos, 1);
        pos = 0;
        assert_eq!(read_varint(&[0xac, 0x02], &mut pos), Some(300));
        assert_eq!(pos, 2);
        pos = 0;
        assert_eq!(read_varint(&[0xff, 0xff, 0xff, 0xff, 0x0f], &mut pos), Some(u32::MAX as usize));
        pos = 0;
        assert_eq!(read_varint(&[0x80, 0x80], &mut pos), None, "truncated");
        pos = 0;
        assert_eq!(read_varint(&[0xff; 8], &mut pos), None, "too long");
        // A data event longer than 127 bytes uses a two-byte length.
        let mut w = W::default();
        w.data(PAT_NOTES, &[7; 200]).byte(CH_TYPE, 3);
        let f = w.file(96);
        let c = parse_container(&f).unwrap();
        assert_eq!(c.events.len(), 2);
        assert_eq!(c.events[0].value, Value::Data(&[7; 200]));
        assert_eq!(c.events[1], Event { id: CH_TYPE, value: Value::Byte(3) });
    }

    #[test]
    fn header_and_value_sizes() {
        let mut w = W::default();
        w.byte(9, 1).word(CH_NEW, 513).dword(TEMPO, 128_000).ascii(FL_VERSION, "9.0");
        let f = w.file(96);
        let c = parse_container(&f).unwrap();
        assert_eq!(c.header, Header { format: 0, channels: 2, ppq: 96 });
        assert_eq!(c.unread, 0);
        let values: Vec<u32> = c.events.iter().map(|e| e.value.int()).collect();
        assert_eq!(values, [1, 513, 128_000, 0]);
    }

    #[test]
    fn text_encodings() {
        assert_eq!(decode_text(&[b'K', 0, b'i', 0, 0xe9, 0, 0, 0], true), "Kié");
        assert_eq!(decode_text(b"Bass\0", false), "Bass");
        assert_eq!(decode_text(&[b'C', 0xe9], false), "Cé");
        // Old project: ASCII names.
        let mut w = W::default();
        w.ascii(FL_VERSION, "9.1.0").word(CH_NEW, 0).byte(CH_TYPE, TYPE_SAMPLER).ascii(CH_NAME_OLD, "Snare");
        let fl = read_project(&w.file(96)).unwrap();
        assert_eq!(fl.channels[0].name.as_deref(), Some("Snare"));
        // FL 12: UTF-16 names, including non-ASCII.
        let mut w = W::default();
        w.ascii(FL_VERSION, "12.5.1").word(CH_NEW, 0).byte(CH_TYPE, TYPE_SAMPLER).text(PLUGIN_NAME, "Clap ♥ ドラム");
        let fl = read_project(&w.file(96)).unwrap();
        assert_eq!(fl.channels[0].name.as_deref(), Some("Clap ♥ ドラム"));
    }

    #[test]
    fn full_import() {
        let resolve = |raw: &str| raw.ends_with("Kick 01.wav").then(|| "/samples/Kick 01.wav".to_string());
        let (p, report) = import_flp(&sample_project(), "file", &resolve).unwrap();
        assert_eq!(p.name, "Night drive");
        assert_eq!(p.bpm, 140.0);

        assert_eq!(p.channels.len(), 2, "the automation clip is skipped");
        let kick = &p.channels[0];
        assert_eq!(kick.name, "Kick");
        assert_eq!(kick.instrument.kind, "sampler");
        assert_eq!(kick.instrument.sample.as_deref(), Some("/samples/Kick 01.wav"));
        assert_eq!(kick.gain, 1.0);
        assert_eq!(kick.pan, -0.5);
        assert_eq!(kick.track, 3);
        assert_eq!(kick.color, 0xff2030);
        let lead = &p.channels[1];
        assert_eq!((lead.name.as_str(), lead.instrument.kind.as_str(), lead.track), ("Lead", "pulse", 4));

        // Pattern 1 at 192 PPQ → 96 PPQ; steps get one step of length.
        assert_eq!(p.patterns.len(), 2);
        let intro = &p.patterns[0];
        assert_eq!((intro.name.as_str(), intro.color, intro.len), ("Intro", 0x00ff00, TICKS_PER_BAR));
        assert_eq!(
            intro.notes[0],
            [
                Note { start: 0, len: TICKS_PER_STEP, key: 60, vel: 100.0 / 128.0 },
                Note { start: 96, len: TICKS_PER_STEP, key: 60, vel: 1.0 }
            ]
        );
        assert_eq!(intro.notes[1], [Note { start: 192, len: 192, key: 64, vel: 0.5 }]);
        assert_eq!(p.patterns[1].name, "Empty");

        // Playlist: two pattern clips, the automation clip is reported.
        assert_eq!(p.mode, PlayMode::Song);
        assert_eq!(p.clips.len(), 2);
        assert_eq!(p.clips[0], ClipDef { pattern: 0, lane: 2, start: 384, len: 384, offset: 0 });
        assert_eq!(p.clips[1].lane, 0);

        // Mixer.
        let drums = &p.tracks[3];
        assert_eq!(drums.name, "Drums");
        assert_eq!((drums.gain, drums.pan, drums.output), (0.5, -0.5, MASTER));
        assert_eq!(drums.sends[0], (1, 0.75));
        let limiter = drums.inserts[0].as_ref().unwrap();
        assert_eq!(limiter.kind, "limiter");
        assert!(limiter.bypass, "slot disabled in the mixer parameters");
        assert_eq!(drums.inserts[1].as_ref().unwrap().kind, "reverb");
        assert!(drums.inserts[2].is_none());
        assert_eq!(p.tracks[4].name, "Insert 4");
        assert!(p.routing_order().is_some());

        let lines = report.lines().join("\n");
        assert!(lines.contains("Mapped: Tempo 140.00 BPM"), "{lines}");
        assert!(lines.contains("Fruity Limiter → "), "{lines}");
        assert!(lines.contains("Approximated: 'Lead': FL generator '3x Osc' replaced by Pulse"), "{lines}");
        assert!(lines.contains("Approximated: 'Drums' slot 3: ValhallaRoom"), "{lines}");
        assert!(lines.contains("Skipped: 'Drums' slot 2: Fruity NoteBook 2 has no Ponzik equivalent"), "{lines}");
        assert!(lines.contains("Skipped: Automation clip 'Cutoff'"), "{lines}");
        assert!(lines.contains("Skipped: 1 automation clips in the playlist"), "{lines}");
        assert!(lines.contains("Skipped: 1 notes on channels"), "{lines}");
        assert!(report.summary().starts_with("FL Studio import: "));
        let rows = report.rows(30);
        assert!(rows.iter().all(|(r, _)| r.chars().count() <= 30 || !r.contains(' ')));
        assert_eq!(rows.iter().filter(|(_, more)| !more).count(), report.lines().len());
        assert_eq!(rows.iter().map(|(r, _)| r.as_str()).collect::<Vec<_>>().join(" "), report.lines().join(" "));
        // The project survives a save / load round trip.
        let back = crate::load(&crate::save(&p)).unwrap();
        assert_eq!(back.channels, p.channels);
        assert_eq!(back.clips, p.clips);
    }

    #[test]
    fn missing_sample_and_audio_clips() {
        let mut w = W::default();
        w.ascii(FL_VERSION, "20.8.4").dword(TEMPO, 120_000);
        w.word(CH_NEW, 0).byte(CH_TYPE, TYPE_INSTRUMENT).text(PLUGIN_INTERNAL_NAME, "").text(PLUGIN_NAME, "Vocal");
        w.text(CH_SAMPLE_PATH, "D:\\vox\\Vocal take.wav");
        w.word(CH_NEW, 1)
            .byte(CH_TYPE, TYPE_SAMPLER)
            .text(PLUGIN_NAME, "Hat")
            .text(CH_SAMPLE_PATH, "%FLStudioFactoryData%\\Hat.wav");
        w.word(CH_NEW, 2).byte(CH_TYPE, TYPE_SAMPLER).text(PLUGIN_NAME, "Loop").text(CH_SAMPLE_PATH, "C:\\loop.mp3");
        w.word(ARR_NEW, 0).data(ARR_PLAYLIST, &item(96 * 8, 0, 96 * 16, 1));
        let resolve = |raw: &str| raw.contains("Vocal").then(|| "/vox/Vocal take.wav".to_string());
        let (p, report) = import_flp(&w.file(96), "Song", &resolve).unwrap();
        assert_eq!(p.name, "Song");
        assert_eq!(p.audio_clips.len(), 1);
        let a = &p.audio_clips[0];
        assert_eq!((a.path.as_str(), a.lane, a.start, a.len), ("/vox/Vocal take.wav", 1, 96 * 8, 96 * 16));
        assert_eq!(p.channels.len(), 2, "the vocal is only an audio clip");
        assert!(p.channels[0].instrument.sample.is_none());
        let lines = report.lines().join("\n");
        assert!(lines.contains("sample not found on this computer: %FLStudioFactoryData%\\Hat.wav"), "{lines}");
        assert!(lines.contains("is not a WAV file"), "{lines}");
    }

    #[test]
    fn routing_loops_are_broken() {
        let mut w = W::default();
        w.ascii(FL_VERSION, "20.8.4");
        for i in 0..3usize {
            w.text(INS_NAME, &format!("Ins {i}")).data(INS_FLAGS, &[0; 12]);
            let mut routes = vec![0u8; 127];
            match i {
                1 => routes[2] = 1,
                2 => routes[1] = 1,
                _ => {}
            }
            w.data(INS_ROUTING, &routes).dword(INS_OUTPUT, 0);
        }
        let (p, report) = import_flp(&w.file(96), "x", &|_| None).unwrap();
        assert!(p.routing_order().is_some());
        assert_eq!((p.tracks[1].output, p.tracks[2].output), (MASTER, MASTER));
        assert!(report.lines().iter().any(|l| l.contains("routing loops")));
    }

    #[test]
    fn fine_tempo_and_old_playlist() {
        let mut w = W::default();
        w.ascii(FL_VERSION, "11.0.0").word(TEMPO_COARSE, 128).word(TEMPO_FINE, 500);
        w.word(PAT_NEW, 1).data(PAT_NOTES, &note(0, 0, 24, 48, 100));
        w.word(CH_NEW, 0).byte(CH_TYPE, TYPE_NATIVE).ascii(PLUGIN_INTERNAL_NAME, "Sytrus");
        // FL < 12.9.1: 199 playlist tracks, stored reversed from 198.
        let mut it = item(0, PATTERN_BASE + 1, 0, 0);
        it[12..14].copy_from_slice(&(198u16 - 3).to_le_bytes());
        w.data(ARR_PLAYLIST, &it);
        let (p, _) = import_flp(&w.file(96), "x", &|_| None).unwrap();
        assert_eq!(p.bpm, 128.5);
        assert_eq!(p.channels[0].instrument.kind, "fm");
        assert_eq!(p.clips[0].lane, 3);
        assert_eq!(p.clips[0].len, TICKS_PER_BAR, "length 0 → the pattern's length");
    }

    #[test]
    fn device_mapping() {
        assert_eq!(effect_for("Fruity Parametric EQ 2"), Some(("eq", true)));
        assert_eq!(effect_for("Fruity Reeverb 2"), Some(("reverb", true)));
        assert_eq!(effect_for("Fruity Delay 3"), Some(("delay", true)));
        assert_eq!(effect_for("FabFilter Pro-Q 3"), Some(("eq", false)));
        assert_eq!(effect_for("Fruity NoteBook 2"), None);
        for name in
            ["3x Osc", "FL Keys", "BooBass", "Fruity Kick", "FPC", "Harmor", "Sakura", "MIDI Out", "Slicex", "Sytrus"]
        {
            assert!(ponzik_engine::registry::instrument_kind(instrument_for(name)).is_some(), "{name}");
        }
        assert_eq!(instrument_for("FL Keys"), "epiano");
        assert_eq!(instrument_for("Serum"), "wavetable");
    }

    #[test]
    fn every_mapped_effect_exists() {
        let names = [
            "fruity parametric eq 2",
            "fruity limiter",
            "fruity compressor",
            "fruity reeverb 2",
            "fruity delay 3",
            "fruity chorus",
            "fruity phaser",
            "fruity filter",
            "fruity balance",
            "fruity stereo enhancer",
            "fruity multiband compressor",
            "fruity soft clipper",
            "fruity squeeze",
            "soundgoodizer",
            "maximus",
            "gross beat",
            "pro-l 2",
            "ott",
            "decapitator",
        ];
        for n in names {
            let (kind, _) = effect_for(n).unwrap_or_else(|| panic!("{n}"));
            assert!(ponzik_engine::registry::effect_kind(kind).is_some(), "{n} → {kind}");
        }
    }

    #[test]
    fn bad_input_never_panics() {
        assert!(import_flp(b"", "x", &|_| None).is_err());
        assert!(import_flp(b"RIFF....WAVE", "x", &|_| None).is_err());
        let mut zero_ppq = sample_project();
        zero_ppq[12..14].copy_from_slice(&0u16.to_le_bytes());
        assert!(import_flp(&zero_ppq, "x", &|_| None).is_err());

        // Every truncation of a valid project.
        let full = sample_project();
        for n in 0..full.len() {
            match import_flp(&full[..n], "x", &|_| None) {
                Ok((_, report)) => assert!(report.skipped.iter().any(|s| s.contains("truncated")), "{n}"),
                Err(_) => assert!(n < 22, "{n}"),
            }
        }
        // Pseudo-random garbage after a valid header, and random corruption of a valid file.
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut rand = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for round in 0..300 {
            let mut f = if round % 2 == 0 { full.clone() } else { W::default().file(96) };
            if round % 2 == 1 {
                let n = (rand() % 2000) as usize;
                f.extend((0..n).map(|_| rand() as u8));
                let len = (f.len() - 22) as u32;
                f[18..22].copy_from_slice(&len.to_le_bytes());
            } else {
                for _ in 0..1 + rand() % 20 {
                    let at = 22 + (rand() as usize) % (f.len() - 22);
                    f[at] = rand() as u8;
                }
            }
            if let Ok((p, _)) = import_flp(&f, "x", &|_| None) {
                let _ = p.sequence();
            }
        }
        // A huge declared data length is clamped to the file.
        let mut big = sample_project();
        big[18..22].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(import_flp(&big, "x", &|_| None).is_ok());
    }

    /// Parses a real project when `PONZIK_FLP_TEST_FILE` points to one (for
    /// example PyFLP's `tests/assets/FL 20.8.4.flp`), and prints the report.
    #[test]
    fn real_file_if_available() {
        let Ok(path) = std::env::var("PONZIK_FLP_TEST_FILE") else {
            return;
        };
        let bytes = std::fs::read(&path).unwrap();
        let dir = Path::new(&path).parent().map(Path::to_path_buf);
        let (p, report) = import_flp(&bytes, "test", &|raw| resolve_sample(raw, dir.as_deref())).unwrap();
        for l in report.lines() {
            println!("{l}");
        }
        println!(
            "{} channels, {} patterns, {} clips, {} audio clips",
            p.channels.len(),
            p.patterns.len(),
            p.clips.len(),
            p.audio_clips.len()
        );
        assert!(p.routing_order().is_some());
    }
}
