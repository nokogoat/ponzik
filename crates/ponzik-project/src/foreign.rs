//! Projects of other programs: which ones Ponzik opens, and the pieces their
//! importers share (REAPER, LMMS, Ardour, MusicXML; FL Studio and Ableton Live
//! have their own modules).
//!
//! [`Build`] assembles the new project: one mixer track per track of the
//! other program, its channels, patterns shared by identical clips, audio
//! clips whose files are looked up once, tempo and markers; it writes the
//! common lines of the import report.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use ponzik_engine::mixer::MASTER;
use ponzik_engine::{Note, PlayMode, TICKS_PER_BAR, TempoPoint};
use ponzik_i18n::{tr, tr_fmt};

use crate::{AudioClipDef, ChannelDef, ClipDef, ImportReport, InstrumentDef, Marker, PatternDef, Project, TimeSig};

/// Clips and notes beyond this tick are dropped (garbage protection).
pub const MAX_TICK: u32 = TICKS_PER_BAR * 4096;
/// A project file larger than this (once unpacked) is refused.
pub const MAX_FILE: usize = 512 << 20;

/// The project formats Ponzik opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Ponzik,
    FlStudio,
    Ableton,
    Reaper,
    Lmms,
    Ardour,
    MusicXml,
}

impl Format {
    /// Every format that is imported (not Ponzik's own).
    pub const IMPORTED: [Format; 6] =
        [Format::FlStudio, Format::Ableton, Format::Reaper, Format::Lmms, Format::Ardour, Format::MusicXml];

    /// The format of a file, by its extension.
    pub fn of(path: &Path) -> Option<Format> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        Some(match ext.as_str() {
            "ponzik" => Format::Ponzik,
            "flp" => Format::FlStudio,
            "als" => Format::Ableton,
            "rpp" => Format::Reaper,
            "mmp" | "mmpz" => Format::Lmms,
            "ardour" => Format::Ardour,
            "musicxml" | "mxl" => Format::MusicXml,
            // A plain .xml file is MusicXML only when it says so.
            "xml" if is_musicxml_file(path) => Format::MusicXml,
            _ => return None,
        })
    }

    /// The program (or notation) the format belongs to.
    pub fn program(self) -> &'static str {
        match self {
            Format::Ponzik => "Ponzik",
            Format::FlStudio => "FL Studio",
            Format::Ableton => "Ableton Live",
            Format::Reaper => "REAPER",
            Format::Lmms => "LMMS",
            Format::Ardour => "Ardour",
            Format::MusicXml => "MusicXML",
        }
    }

    /// The extensions of the format, the usual one first.
    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            Format::Ponzik => &["ponzik"],
            Format::FlStudio => &["flp"],
            Format::Ableton => &["als"],
            Format::Reaper => &["rpp"],
            Format::Lmms => &["mmp", "mmpz"],
            Format::Ardour => &["ardour"],
            Format::MusicXml => &["musicxml", "mxl"],
        }
    }
}

/// True for an `.xml` file that starts like a MusicXML score.
fn is_musicxml_file(path: &Path) -> bool {
    use std::io::Read;
    let mut head = [0u8; 1024];
    let n = std::fs::File::open(path).and_then(|mut f| f.read(&mut head)).unwrap_or(0);
    let text = String::from_utf8_lossy(&head[..n]);
    text.contains("<score-partwise") || text.contains("<score-timewise") || text.contains("MusicXML")
}

/// Projects of programs Ponzik knows but cannot open yet, and why: the
/// message shown when one is opened.
pub fn unsupported(path: &Path) -> Option<String> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let (program, why) = match ext.as_str() {
        "bwproject" => ("Bitwig Studio", tr("its project format is closed and binary")),
        "song" => ("Studio One", tr("its song format is not documented")),
        "cpr" => ("Cubase", tr("its project format is closed and binary")),
        "logicx" => ("Logic Pro", tr("its project format is closed and binary")),
        "ptx" => ("Pro Tools", tr("its project format is closed and binary")),
        "reason" => ("Reason", tr("its project format is closed and binary")),
        _ => return None,
    };
    Some(tr_fmt!("{} projects can't be opened yet ({}): export MIDI files and audio stems from {} instead", program, why, program))
}

/// Opens the project file `path` of another program as a new Ponzik
/// project; its media are looked up next to it too.
pub fn import_file(path: &Path) -> Result<(Project, ImportReport), String> {
    let format = Format::of(path).filter(|f| *f != Format::Ponzik).ok_or_else(|| {
        unsupported(path).unwrap_or_else(|| tr_fmt!("{}: not a project Ponzik can open", path.display().to_string()))
    })?;
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let name = path.file_stem().map_or_else(|| format.program().to_string(), |s| s.to_string_lossy().into_owned());
    let dir = path.parent();
    let resolve = |raw: &str| crate::flp::resolve_sample(raw, dir);
    match format {
        Format::FlStudio => crate::flp::import_flp(&bytes, &name, &resolve),
        Format::Ableton => crate::als::import_als(&bytes, &name, &resolve),
        Format::Reaper => crate::rpp::import_rpp(&bytes, &name, &resolve),
        Format::Lmms => crate::lmms::import_lmms(&bytes, &name, &|raw| crate::lmms::resolve_lmms_sample(raw, dir)),
        Format::Ardour => crate::ardour::import_ardour(&bytes, &name, dir, &resolve),
        Format::MusicXml => crate::musicxml::import_musicxml(&bytes, &name),
        Format::Ponzik => unreachable!("filtered out"),
    }
}

// --- Instruments ---

/// The closest native instrument for an instrument name: a General MIDI or
/// orchestral name, a plugin or a track called after what it plays.
pub fn instrument_for_name(name: &str) -> &'static str {
    let n = name.to_lowercase();
    let has = |keys: &[&str]| keys.iter().any(|k| n.contains(k));
    if has(&["rhodes"]) {
        "rhodes"
    } else if has(&["wurli"]) {
        "wurli"
    } else if has(&["e-piano", "electric piano", "epiano", "e.piano"]) {
        "epiano"
    } else if has(&["piano", "pianoteq", "grand", "fluid", "general midi", "gm synth", "sf2", "soundfont", "sfz"]) {
        "piano"
    } else if has(&["organ", "hammond", "setbfree"]) {
        "organ"
    } else if has(&["kick", "bass drum"]) {
        "kick"
    } else if has(&["drum", "hydrogen", "drumgizmo", "perc"]) {
        "drum_machine"
    } else if has(&["guitar", "gtr", "banjo", "mandolin", "ukulele"]) {
        "guitar"
    } else if has(&["violin", "viola", "cello", "contrabass", "string", "orchestra", "ensemble", "choir"]) {
        "strings"
    } else if has(&["trumpet", "trombone", "horn", "tuba", "brass", "cornet", "flugel"]) {
        "brass"
    } else if has(&["flute", "clarinet", "oboe", "sax", "bassoon", "piccolo", "recorder", "wind", "reed"]) {
        "winds"
    } else if has(&["marimba", "vibraphone", "xylophone", "glockenspiel", "bell", "celesta", "mallet", "kalimba"]) {
        "mallets"
    } else if has(&["harp", "pizz", "koto", "sitar", "harpsichord", "clavinet"]) {
        "pluck"
    } else if has(&["samplomatic", "sampler", "audiofileprocessor", "sfizz"]) {
        "sampler"
    } else if has(&["dexed", "fm8", "opl", "fm synth"]) {
        "fm"
    } else if has(&["helm", "surge", "pad"]) {
        "wavetable"
    } else {
        crate::flp::instrument_for(name)
    }
}

/// The closest native instrument for a General MIDI program (0 to 127) on a
/// MIDI channel (0 to 15; 9 plays drums).
pub fn instrument_for_program(program: u8, channel: u8) -> &'static str {
    if channel == 9 {
        return "drum_machine";
    }
    match program {
        0..=3 | 6 => "piano",
        4 => "rhodes",
        5 => "epiano",
        7 => "pluck",
        8..=15 => "mallets",
        16..=23 => "organ",
        24..=31 => "guitar",
        32..=37 => "pulse",
        38..=39 => "bass303",
        40..=44 | 48..=55 => "strings",
        45 | 46 | 104..=111 => "pluck",
        47 => "drumsynth",
        56..=63 => "brass",
        64..=79 => "winds",
        80..=87 => "pulse",
        88..=103 => "wavetable",
        112..=119 => "drumsynth",
        _ => "pulse",
    }
}

/// The English name of a native instrument (for report lines).
pub fn instrument_name(kind: &str) -> &'static str {
    ponzik_engine::registry::instrument_kind(kind).map_or("?", |k| k.name)
}

// --- Building the project ---

/// A project being imported.
pub struct Build {
    pub p: Project,
    pub report: ImportReport,
    /// Identical single-channel clips share a pattern: (channel, notes, length) → pattern.
    shared: Vec<(usize, Vec<Note>, u32, usize)>,
    /// Next mixer track to give (the empty ones of a new project first).
    next_track: usize,
    /// Media looked up: path in the file → path kept.
    media: BTreeMap<String, String>,
    offline: BTreeSet<String>,
    /// Counts for the report.
    pub note_count: usize,
    pub midi_clips: usize,
    pub tracks_made: usize,
}

impl Build {
    pub fn new(source: &str, name: &str) -> Self {
        let mut p = Project::empty();
        p.name = name.to_string();
        p.patterns.clear();
        Self {
            p,
            report: ImportReport { source: source.into(), ..Default::default() },
            shared: Vec::new(),
            next_track: 1,
            media: BTreeMap::new(),
            offline: BTreeSet::new(),
            note_count: 0,
            midi_clips: 0,
            tracks_made: 0,
        }
    }

    /// A mixer track for a track of the other program; the master when the
    /// mixer is full.
    pub fn track(&mut self, name: &str, color: u32, gain: f32, pan: f32, muted: bool) -> usize {
        while self.p.tracks.len() <= self.next_track {
            if self.p.add_track().is_none() {
                return MASTER;
            }
        }
        let t = self.next_track;
        self.next_track += 1;
        self.tracks_made += 1;
        let tr = &mut self.p.tracks[t];
        tr.name = name.to_string();
        tr.color = color;
        tr.gain = finite(gain, 1.0).clamp(0.0, 4.0);
        tr.pan = finite(pan, 0.0).clamp(-1.0, 1.0);
        tr.muted = muted;
        t
    }

    /// A channel playing `instrument` into mixer track `track`.
    pub fn channel(&mut self, name: &str, instrument: InstrumentDef, track: usize, color: u32) -> usize {
        self.p.channels.push(ChannelDef {
            name: name.to_string(),
            instrument,
            note_fx: Vec::new(),
            gain: 1.0,
            pan: 0.0,
            muted: false,
            track,
            root_note: 60,
            color,
            modulators: Vec::new(),
            frozen: None,
            porta: 0.0,
        });
        self.p.channels.len() - 1
    }

    /// The pattern of `notes` on channel `ch`, `len` ticks long: an existing
    /// one with the same notes, or a new one.
    pub fn pattern(&mut self, ch: usize, mut notes: Vec<Note>, len: u32, name: &str, color: u32) -> usize {
        let len = len.clamp(1, MAX_TICK);
        notes.retain(|n| n.start < len);
        for n in &mut notes {
            n.len = n.len.clamp(1, len - n.start);
        }
        notes.sort_by_key(|n| (n.start, n.key));
        if let Some(s) = self.shared.iter().find(|s| s.0 == ch && s.1 == notes && s.2 == len) {
            return s.3;
        }
        self.note_count += notes.len();
        let mut per = vec![Vec::new(); ch + 1];
        per[ch] = notes.clone();
        self.p.patterns.push(PatternDef { name: name.to_string(), len, notes: per, color, groove: None });
        let at = self.p.patterns.len() - 1;
        self.shared.push((ch, notes, len, at));
        at
    }

    /// A pattern with notes on several channels (a beat pattern, a score).
    pub fn multi_pattern(&mut self, notes: Vec<(usize, Vec<Note>)>, len: u32, name: &str, color: u32) -> usize {
        let len = len.clamp(1, MAX_TICK);
        let mut per: Vec<Vec<Note>> = Vec::new();
        for (ch, mut list) in notes {
            if per.len() <= ch {
                per.resize(ch + 1, Vec::new());
            }
            list.retain(|n| n.start < len);
            for n in &mut list {
                n.len = n.len.clamp(1, len - n.start);
            }
            per[ch].append(&mut list);
            per[ch].sort_by_key(|n| (n.start, n.key));
        }
        self.note_count += per.iter().map(Vec::len).sum::<usize>();
        self.p.patterns.push(PatternDef { name: name.to_string(), len, notes: per, color, groove: None });
        self.p.patterns.len() - 1
    }

    /// Places `pattern` in the playlist (dropped beyond [`MAX_TICK`]).
    pub fn clip(&mut self, pattern: usize, lane: usize, start: u32, len: u32, offset: u32, muted: bool) {
        if start >= MAX_TICK {
            return;
        }
        let len = len.clamp(1, MAX_TICK - start);
        self.p.clips.push(ClipDef { pattern, lane, start, len, offset, muted });
        self.midi_clips += 1;
    }

    /// Places an audio clip (dropped beyond [`MAX_TICK`]).
    pub fn audio(&mut self, mut clip: AudioClipDef) {
        if clip.start >= MAX_TICK || clip.path.is_empty() {
            return;
        }
        clip.len = clip.len.clamp(1, MAX_TICK - clip.start);
        clip.offset = if clip.offset.is_finite() { clip.offset.max(0.0) } else { 0.0 };
        clip.gain = finite(clip.gain, 1.0).clamp(0.0, 4.0);
        clip.fade_in = finite(clip.fade_in, 0.0).clamp(0.0, 600.0);
        clip.fade_out = finite(clip.fade_out, 0.0).clamp(0.0, 600.0);
        self.p.audio_clips.push(clip);
    }

    /// The path the project keeps for the media file `raw` of the other
    /// project, looked up once through `resolve`; a missing file (or one in
    /// a format not read yet) stays in the project, offline. With `factory`,
    /// a missing file is replaced by the closest library sound.
    pub fn locate(&mut self, raw: &str, resolve: &dyn Fn(&str) -> Option<String>, factory: Option<&str>) -> String {
        if let Some(path) = self.media.get(raw) {
            return path.clone();
        }
        let file = crate::flp::file_name_of(raw);
        let readable = ponzik_engine::wav::is_audio_file(Path::new(file));
        let path = match resolve(raw) {
            Some(found) if readable => {
                self.media.insert(raw.to_string(), found.clone());
                return found;
            }
            Some(found) => {
                self.report.offline.push(tr_fmt!("{} (format not yet supported)", raw));
                found
            }
            None => match factory.and_then(|_| crate::library::closest_library_sound(file)) {
                Some(sound) => {
                    let name = crate::flp::file_name_of(&sound).trim_end_matches(".wav");
                    self.report.approximated.push(tr_fmt!(
                        "{} factory sample {} replaced by library sound {}",
                        factory.unwrap_or(""),
                        file,
                        name
                    ));
                    self.media.insert(raw.to_string(), sound.clone());
                    return sound;
                }
                None if readable => {
                    self.report.offline.push(tr_fmt!("{} not found on this computer", raw));
                    raw.to_string()
                }
                None => {
                    self.report.offline.push(tr_fmt!("{} (format not yet supported)", raw));
                    raw.to_string()
                }
            },
        };
        self.media.insert(raw.to_string(), path.clone());
        self.offline.insert(path.clone());
        path
    }

    /// The tempo: the first one and the changes after it, (tick, BPM, ramp
    /// from the previous point).
    pub fn tempo(&mut self, first: f64, changes: &[(u32, f64, bool)]) {
        if first.is_finite() && first > 0.0 {
            self.p.bpm = (first as f32).clamp(10.0, 999.0);
            self.report.mapped.push(tr_fmt!("Tempo {:.2} BPM", self.p.bpm));
        } else {
            self.report.approximated.push(tr("No tempo found: 120 BPM").into());
        }
        let mut points: Vec<TempoPoint> = changes
            .iter()
            .filter(|c| c.0 > 0 && c.0 < MAX_TICK && c.1.is_finite() && c.1 > 0.0)
            .map(|&(tick, bpm, ramp)| TempoPoint { tick, bpm: (bpm as f32).clamp(10.0, 999.0), ramp })
            .collect();
        points.sort_by_key(|p| p.tick);
        points.dedup_by_key(|p| p.tick);
        // Repeats of the tempo in force are no change.
        let mut last = (self.p.bpm, false);
        points.retain(|p| {
            let keep = p.bpm != last.0 || p.ramp;
            last = (p.bpm, p.ramp);
            keep
        });
        if !points.is_empty() {
            self.report.mapped.push(tr_fmt!("{} tempo changes", points.len()));
        }
        self.p.tempo_changes = points;
    }

    /// A time signature from `tick` (bars restart there).
    pub fn signature(&mut self, tick: u32, num: u32, den: u32) {
        if !(1..=64).contains(&num) || !TimeSig::valid_den(den.min(255) as u8) || tick >= MAX_TICK {
            return;
        }
        let sig = TimeSig { tick, num: num as u8, den: den as u8 };
        self.p.signatures.retain(|s| s.tick != tick);
        self.p.signatures.push(sig);
        self.p.signatures.sort_by_key(|s| s.tick);
    }

    pub fn marker(&mut self, tick: u32, name: &str, color: u32) {
        if tick < MAX_TICK {
            self.p.markers.push(Marker { tick, name: name.trim().to_string(), color });
        }
    }

    /// The finished project and its report.
    pub fn finish(mut self) -> (Project, ImportReport) {
        let p = &mut self.p;
        // 4/4 from the start is the default: no signature needed for it.
        p.signatures.dedup_by(|b, a| a.same_meter(*b));
        if p.signatures.first().is_some_and(|s| s.tick == 0 && s.num == 4 && s.den == 4) {
            p.signatures.remove(0);
        }
        let sigs: Vec<String> = p.signatures.iter().map(|s| format!("{}/{}", s.num, s.den)).collect();
        if !sigs.is_empty() {
            self.report.mapped.push(tr_fmt!("Time signatures: {}", sigs.join(", ")));
        }
        for pat in &mut p.patterns {
            pat.notes.resize(p.channels.len(), Vec::new());
        }
        if p.patterns.is_empty() {
            p.patterns.push(Project::empty().patterns.remove(0));
        }
        p.markers.sort_by_key(|m| m.tick);
        p.markers.dedup_by_key(|m| m.tick);
        if self.tracks_made > 0 {
            self.report.mapped.insert(0, tr_fmt!("{} tracks: name, colour, volume, pan, mute", self.tracks_made));
        }
        if !p.channels.is_empty() {
            let kinds: BTreeSet<&str> = p.channels.iter().map(|c| instrument_name(&c.instrument.kind)).collect();
            let kinds: Vec<&str> = kinds.into_iter().collect();
            self.report.approximated.push(tr_fmt!(
                "{} instrument channels, each with the closest Ponzik instrument ({})",
                p.channels.len(),
                kinds.join(", ")
            ));
        }
        if self.midi_clips > 0 {
            self.report.mapped.push(tr_fmt!(
                "{} patterns from {} MIDI clips, {} notes",
                p.patterns.len(),
                self.midi_clips,
                self.note_count
            ));
        }
        if !p.audio_clips.is_empty() {
            self.report.mapped.push(tr_fmt!(
                "{} audio clips: file, place, length, start in the file, gain, fades",
                p.audio_clips.len()
            ));
        }
        if !p.markers.is_empty() {
            self.report.mapped.push(tr_fmt!("{} markers", p.markers.len()));
        }
        if !p.clips.is_empty() || !p.audio_clips.is_empty() {
            p.mode = PlayMode::Song;
        }
        p.clamp_track_refs();
        (self.p, self.report)
    }
}

/// `v` when finite, else `or`.
pub fn finite<T: Into<f64> + Copy>(v: T, or: f32) -> f32 {
    let x: f64 = v.into();
    if x.is_finite() { x as f32 } else { or }
}

/// A non-negative tick count from a float (0 for garbage), at most [`MAX_TICK`].
pub fn to_tick(t: f64) -> u32 {
    if t.is_finite() && t > 0.0 { t.round().min(MAX_TICK as f64) as u32 } else { 0 }
}

/// A colour written `#rrggbb` (or `rrggbb`, `#rgb`); 0 when it is not one.
pub fn hex_color(s: &str) -> u32 {
    let h = s.trim().trim_start_matches('#');
    match h.len() {
        6 | 8 => u32::from_str_radix(&h[..6], 16).unwrap_or(0),
        3 => u32::from_str_radix(h, 16).map_or(0, |v| {
            let (r, g, b) = ((v >> 8) & 15, (v >> 4) & 15, v & 15);
            ((r * 17) << 16) | ((g * 17) << 8) | (b * 17)
        }),
        _ => 0,
    }
}

/// Routes mixer track `from` into `to` unless that makes a loop.
pub fn route(p: &mut Project, from: usize, to: usize) -> bool {
    let n = p.tracks.len();
    if from == MASTER || from >= n || to >= n || from == to {
        return false;
    }
    // Follow the outputs from `to`: reaching `from` would be a loop.
    let mut at = to;
    for _ in 0..n {
        if at == MASTER {
            p.tracks[from].output = to;
            return true;
        }
        if at == from {
            return false;
        }
        at = p.tracks[at].output;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_by_extension() {
        assert_eq!(Format::of(Path::new("/a/Song.RPP")), Some(Format::Reaper));
        assert_eq!(Format::of(Path::new("a.mmpz")), Some(Format::Lmms));
        assert_eq!(Format::of(Path::new("a.mmp")), Some(Format::Lmms));
        assert_eq!(Format::of(Path::new("a.ardour")), Some(Format::Ardour));
        assert_eq!(Format::of(Path::new("a.mxl")), Some(Format::MusicXml));
        assert_eq!(Format::of(Path::new("a.musicxml")), Some(Format::MusicXml));
        assert_eq!(Format::of(Path::new("a.flp")), Some(Format::FlStudio));
        assert_eq!(Format::of(Path::new("a.ponzik")), Some(Format::Ponzik));
        assert_eq!(Format::of(Path::new("/nonexistent/a.xml")), None);
        assert_eq!(Format::of(Path::new("a.wav")), None);
        for f in Format::IMPORTED {
            for e in f.extensions() {
                assert_eq!(Format::of(Path::new(&format!("x.{e}"))), Some(f));
            }
        }
        assert!(unsupported(Path::new("a.bwproject")).unwrap().contains("Bitwig"));
        assert!(unsupported(Path::new("a.song")).unwrap().contains("Studio One"));
        assert!(unsupported(Path::new("a.rpp")).is_none());
        assert!(import_file(Path::new("/nonexistent/a.song")).unwrap_err().contains("Studio One"));
    }

    #[test]
    fn instruments_from_names_and_programs() {
        assert_eq!(instrument_for_name("Grand Piano"), "piano");
        assert_eq!(instrument_for_name("Violin I"), "strings");
        assert_eq!(instrument_for_name("Trumpet in Bb"), "brass");
        assert_eq!(instrument_for_name("Drums"), "drum_machine");
        assert_eq!(instrument_for_name("Kick"), "kick");
        assert_eq!(instrument_for_name("VSTi: Serum (Xfer Records)"), "wavetable");
        assert_eq!(instrument_for_name("ReaSynth"), "pulse");
        assert_eq!(instrument_for_program(0, 0), "piano");
        assert_eq!(instrument_for_program(40, 0), "strings");
        assert_eq!(instrument_for_program(33, 9), "drum_machine");
        for prog in 0..128 {
            assert!(ponzik_engine::registry::instrument_kind(instrument_for_program(prog, 0)).is_some());
        }
    }

    #[test]
    fn identical_clips_share_a_pattern_and_routes_never_loop() {
        let mut b = Build::new("Test", "x");
        let t = b.track("Bass", 0x123456, 0.5, -0.25, true);
        assert_eq!(t, 1);
        let ch = b.channel("Bass", InstrumentDef::new("pulse"), t, 0);
        let notes = vec![Note { start: 0, len: 9999, key: 40, ..Note::DEFAULT }, Note { start: 500, ..Note::DEFAULT }];
        let a = b.pattern(ch, notes.clone(), 384, "A", 0);
        assert_eq!(b.pattern(ch, notes, 384, "A again", 0), a);
        b.clip(a, 0, 0, 384, 0, false);
        b.clip(a, 0, MAX_TICK, 384, 0, false);
        let (p, report) = b.finish();
        assert_eq!(p.patterns.len(), 1);
        assert_eq!(p.patterns[0].notes[0].len(), 1, "notes past the end go");
        assert_eq!(p.patterns[0].notes[0][0].len, 384, "notes end with the pattern");
        assert_eq!(p.clips.len(), 1);
        assert_eq!((p.tracks[1].gain, p.tracks[1].pan, p.tracks[1].muted), (0.5, -0.25, true));
        assert!(report.mapped[0].contains("1 tracks"));
        assert_eq!(p.mode, PlayMode::Song);

        let mut p = Project::empty();
        assert!(route(&mut p, 2, 3));
        assert!(route(&mut p, 3, 4));
        assert!(!route(&mut p, 4, 2), "a loop");
        assert!(!route(&mut p, MASTER, 2));
        assert_eq!(hex_color("#ff8000"), 0xff8000);
        assert_eq!(hex_color("#f80"), 0xff8800);
        assert_eq!(hex_color("junk"), 0);
    }
}
