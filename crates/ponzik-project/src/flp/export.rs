//! FL Studio project export (`.flp`): an interchange copy of a Ponzik project.
//!
//! Written with the same event ids and layouts as the reader (see the parent
//! module and PyFLP), in the order FL Studio 20 writes them: project info,
//! pattern notes, channels, pattern names, the playlist, then the mixer.
//!
//! Kept: tempo, time signature, title, channels (name, colour, volume, pan,
//! mixer track, root note, mute; samplers keep their sample path, other
//! instruments become a 3x Osc), patterns (name, colour, length, notes with
//! velocity), playlist clips of patterns and audio files, mixer tracks (name,
//! colour, volume, pan, mute, output and sends). Lost: Ponzik's instruments
//! and effects and their settings, note effects, automation, tempo changes,
//! markers, the Session view, takes, warping and fades. An Instrument Rack
//! exports as its first chain's instrument.

use std::collections::BTreeMap;

use ponzik_engine::mixer::MASTER;

use super::*;

/// Version written in the file: FL Studio 20.8 reads and upgrades it, and
/// later versions open it as an older project.
const VERSION: &str = "20.8.4.2576";
const FL_BUILD: u8 = DWORD + 31;
/// FL Studio's mixer: the master, 125 inserts and the "current" insert.
const FL_INSERTS: usize = 127;
/// Insert flags (PyFLP `InsertFlags`): effects on, insert on, docked in the middle.
const INSERT_ENABLE_FX: u32 = 4;
const INSERT_ENABLED: u32 = 8;
const INSERT_DOCK_MIDDLE: u32 = 64;
/// The instrument other than the Sampler that FL Studio always has.
const GENERATOR: &str = "3x Osc";

/// FL Studio event stream being written.
#[derive(Default)]
struct Writer(Vec<u8>);

impl Writer {
    fn byte(&mut self, id: u8, v: u8) {
        self.0.extend_from_slice(&[id, v]);
    }
    fn word(&mut self, id: u8, v: u16) {
        self.0.push(id);
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn dword(&mut self, id: u8, v: u32) {
        self.0.push(id);
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn data(&mut self, id: u8, d: &[u8]) {
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
    }
    fn ascii(&mut self, id: u8, s: &str) {
        let mut d: Vec<u8> = s.chars().map(|c| if c.is_ascii() { c as u8 } else { b'?' }).collect();
        d.push(0);
        self.data(id, &d);
    }
    /// UTF-16LE text with its NUL, as FL Studio 11.5 and later write it.
    fn text(&mut self, id: u8, s: &str) {
        let mut d: Vec<u8> = s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        d.extend_from_slice(&[0, 0]);
        self.data(id, &d);
    }
}

/// `0xRRGGBB` → RGBA bytes read little-endian (the reverse of [`rgb`]).
fn color_event(c: u32) -> u32 {
    let [_, r, g, b] = c.to_be_bytes();
    u32::from_le_bytes([r, g, b, 0])
}

fn level(gain: f32) -> u32 {
    (gain.clamp(0.0, 1.25) * FL_UNITY).round() as u32
}

/// Pan in [-1, 1] → FL's channel pan, 0 to 12 800 around 6 400.
fn channel_pan(pan: f32) -> u32 {
    ((pan.clamp(-1.0, 1.0) + 1.0) * 6400.0).round() as u32
}

/// A playlist item (32 bytes, FL Studio 20 layout).
fn item(pos: u32, index: u16, len: u32, lane: usize, start: u32, end: u32) -> [u8; 32] {
    let mut it = [0u8; 32];
    it[0..4].copy_from_slice(&pos.to_le_bytes());
    it[4..6].copy_from_slice(&PATTERN_BASE.to_le_bytes());
    it[6..8].copy_from_slice(&index.to_le_bytes());
    it[8..12].copy_from_slice(&len.to_le_bytes());
    let track = (FL_PLAYLIST_TRACKS - 1).saturating_sub(lane.min(FL_PLAYLIST_TRACKS - 1)) as u16;
    it[12..14].copy_from_slice(&track.to_le_bytes());
    // Group 0, then the bytes FL Studio writes for a plain item.
    it[16..24].copy_from_slice(&[120, 0, 64, 0, 64, 100, 128, 128]);
    it[24..28].copy_from_slice(&start.to_le_bytes());
    it[28..32].copy_from_slice(&end.to_le_bytes());
    it
}

/// One note (24 bytes, PyFLP `NotesEvent`).
fn note(n: &Note, channel: u16) -> [u8; 24] {
    let mut d = [0u8; 24];
    d[0..4].copy_from_slice(&n.start.to_le_bytes());
    d[4..6].copy_from_slice(&0x4000u16.to_le_bytes());
    d[6..8].copy_from_slice(&channel.to_le_bytes());
    d[8..12].copy_from_slice(&n.len.max(1).to_le_bytes());
    d[12..14].copy_from_slice(&(n.key.min(127) as u16).to_le_bytes());
    // Group, fine pitch (120: none), release, MIDI channel, pan, velocity, mod X and Y.
    d[14..24].copy_from_slice(&[0, 0, 120, 0, 64, 0, 64, (n.vel.clamp(0.0, 1.0) * 128.0).round() as u8, 128, 128]);
    d
}

/// Writes `project` as an FL Studio project. `media` turns a sample path of
/// the project into the path to write (an absolute path on disk), or `None`
/// to keep it as it is.
pub fn export_flp(project: &Project, media: &dyn Fn(&str) -> Option<String>) -> Vec<u8> {
    let p = project;
    let mut w = Writer::default();
    let file_path = |raw: &str| media(raw).unwrap_or_else(|| raw.to_string());

    // Project.
    w.ascii(FL_VERSION, VERSION);
    w.dword(FL_BUILD, 2576);
    w.dword(TEMPO, (p.bpm.clamp(10.0, 999.0) * 1000.0).round() as u32);
    let sig = p.signatures.iter().find(|s| s.tick == 0).map_or((4, 4), |s| (s.num, s.den));
    w.byte(TIME_SIG_NUM, sig.0);
    w.byte(TIME_SIG_BEAT, sig.1);
    w.text(TITLE, &p.name);

    // Sources of audio clips: one Sampler channel per file and mixer track,
    // after the instrument channels.
    let mut sources: BTreeMap<(String, usize), u16> = BTreeMap::new();
    for c in &p.audio_clips {
        let next = (p.channels.len() + sources.len()) as u16;
        sources.entry((c.path.clone(), c.track)).or_insert(next);
    }

    // Pattern notes (FL Studio 20 writes them before the channels).
    for (i, pat) in p.patterns.iter().enumerate() {
        let mut notes: Vec<(u32, [u8; 24])> = Vec::new();
        for (ch, list) in pat.notes.iter().enumerate().take(p.channels.len()) {
            notes.extend(list.iter().filter(|n| !n.slide).map(|n| (n.start, note(n, ch as u16))));
        }
        if notes.is_empty() {
            continue;
        }
        notes.sort_by_key(|n| n.0);
        w.word(PAT_NEW, i as u16 + 1);
        w.data(PAT_NOTES, &notes.iter().flat_map(|n| n.1).collect::<Vec<u8>>());
    }

    // Channels.
    let track = |t: usize| t.min(FL_INSERTS - 2) as u8;
    for (i, c) in p.channels.iter().enumerate() {
        // An Instrument Rack: its first chain's instrument stands for it.
        let inst = c.instrument.first_layer().unwrap_or_else(|| InstrumentDef::new("pulse"));
        let sampler = inst.kind == "sampler";
        w.word(CH_NEW, i as u16);
        w.byte(CH_TYPE, if sampler { TYPE_SAMPLER } else { TYPE_NATIVE });
        w.text(PLUGIN_INTERNAL_NAME, if sampler { "" } else { GENERATOR });
        w.text(PLUGIN_NAME, &c.name);
        if c.color != 0 {
            w.dword(PLUGIN_COLOR, color_event(c.color));
        }
        w.byte(CH_ENABLED, u8::from(!c.muted));
        if let Some(sample) = inst.sample.as_deref().filter(|_| sampler) {
            w.text(CH_SAMPLE_PATH, &file_path(sample));
        }
        w.dword(CH_ROOT_NOTE, c.root_note.min(127) as u32);
        w.byte(CH_ROUTED_TO, track(c.track));
        w.data(CH_LEVELS, &levels(c.pan, c.gain));
    }
    for ((path, t), &iid) in &sources {
        let name = file_name_of(path);
        let name = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
        w.word(CH_NEW, iid);
        w.byte(CH_TYPE, TYPE_SAMPLER);
        w.text(PLUGIN_INTERNAL_NAME, "");
        w.text(PLUGIN_NAME, name);
        w.byte(CH_ENABLED, 1);
        w.text(CH_SAMPLE_PATH, &file_path(path));
        w.byte(CH_ROUTED_TO, track(*t));
        w.data(CH_LEVELS, &levels(0.0, 0.78));
    }

    // Pattern names, colours and lengths.
    for (i, pat) in p.patterns.iter().enumerate() {
        w.word(PAT_NEW, i as u16 + 1);
        w.text(PAT_NAME, &pat.name);
        if pat.color != 0 {
            w.dword(PAT_COLOR, color_event(pat.color));
        }
        w.dword(PAT_LENGTH, pat.len);
    }

    // Playlist.
    w.word(ARR_NEW, 0);
    w.text(ARR_NAME, "Arrangement");
    let mut items: Vec<[u8; 32]> = Vec::new();
    // Muted clips stay out: Ponzik keeps them aside, FL Studio would play them.
    for c in p.clips.iter().filter(|c| c.pattern < p.patterns.len() && !c.muted) {
        let (start, end) = if c.offset > 0 { (c.offset, c.offset + c.len) } else { (u32::MAX, u32::MAX) };
        items.push(item(c.start, PATTERN_BASE + c.pattern as u16 + 1, c.len, c.lane, start, end));
    }
    for c in p.audio_clips.iter().filter(|c| !c.muted) {
        let iid = sources[&(c.path.clone(), c.track)];
        let start = if c.offset > 0.0 { (c.offset * 1000.0) as f32 } else { -1.0 };
        items.push(item(c.start, iid, c.len, c.lane, start.to_bits(), (-1.0f32).to_bits()));
    }
    items.sort_by_key(|it| u32::from_le_bytes([it[0], it[1], it[2], it[3]]));
    w.data(ARR_PLAYLIST, &items.concat());
    w.word(ARR_CURRENT, 0);

    // Mixer: every insert, as FL Studio writes them.
    let mut params = Vec::new();
    let mut param = |id: u8, insert: usize, msg: i32| {
        params.extend_from_slice(&[0, 0, 0, 0, id, 0]);
        params.extend_from_slice(&((insert as u16) << 6).to_le_bytes());
        params.extend_from_slice(&msg.to_le_bytes());
    };
    for i in 0..FL_INSERTS {
        let t = p.tracks.get(i).filter(|_| i < FL_INSERTS - 1);
        if let Some(t) = t {
            if i != MASTER {
                w.text(INS_NAME, &t.name);
            }
            if t.color != 0 {
                w.dword(INS_COLOR, color_event(t.color));
            }
        }
        let muted = t.is_some_and(|t| t.muted);
        let flags = INSERT_ENABLE_FX | INSERT_DOCK_MIDDLE | if muted { 0 } else { INSERT_ENABLED };
        let mut f = [0u8; 12];
        f[4..8].copy_from_slice(&flags.to_le_bytes());
        w.data(INS_FLAGS, &f);
        let mut routes = [0u8; FL_INSERTS];
        if i != MASTER {
            match t {
                Some(t) => {
                    let out = if t.output < FL_INSERTS - 1 && t.output != i { t.output } else { MASTER };
                    routes[out] = 1;
                    param(PARAM_ROUTE_START + out as u8, i, FL_UNITY as i32);
                    for &(to, lvl) in t.sends.iter().filter(|s| s.1 > 0.0 && s.0 != i && s.0 != out && s.0 < FL_INSERTS - 1) {
                        routes[to] = 1;
                        param(PARAM_ROUTE_START + to as u8, i, level(lvl) as i32);
                    }
                }
                None => routes[MASTER] = 1,
            }
        }
        w.data(INS_ROUTING, &routes);
        w.dword(INS_OUTPUT, u32::MAX);
        if let Some(t) = t {
            param(PARAM_VOLUME, i, level(t.gain) as i32);
            param(PARAM_PAN, i, (t.pan.clamp(-1.0, 1.0) * 6400.0).round() as i32);
        }
    }
    w.data(MIXER_PARAMS, &params);

    let channels = (p.channels.len() + sources.len()) as u16;
    let mut f = b"FLhd".to_vec();
    f.extend_from_slice(&6u32.to_le_bytes());
    f.extend_from_slice(&0i16.to_le_bytes());
    f.extend_from_slice(&channels.to_le_bytes());
    f.extend_from_slice(&(PPQ as u16).to_le_bytes());
    f.extend_from_slice(b"FLdt");
    f.extend_from_slice(&(w.0.len() as u32).to_le_bytes());
    f.extend_from_slice(&w.0);
    f
}

/// Channel levels (PyFLP `LevelsEvent`): pan, volume, pitch shift, filter
/// cutoff and resonance (fully open), filter type.
fn levels(pan: f32, gain: f32) -> [u8; 24] {
    let mut d = [0u8; 24];
    d[0..4].copy_from_slice(&channel_pan(pan).to_le_bytes());
    d[4..8].copy_from_slice(&level(gain).to_le_bytes());
    d[12..16].copy_from_slice(&256u32.to_le_bytes());
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_instrument_rack_exports_as_its_first_chain() {
        let mut p = Project::empty();
        let mut sampler = InstrumentDef::new("sampler");
        sampler.sample = Some("/samples/piano.wav".into());
        let mut rack = sampler.into_layer_rack();
        rack.layer_add_chain(Some("strings"));
        p.add_channel("Layers", rack, 60);
        p.add_channel("Empty", InstrumentDef::new_layer_rack(), 60);
        let (q, _) = import_flp(&export_flp(&p, &|_| None), "x", &|raw| Some(raw.to_string())).unwrap();
        assert_eq!(q.channels[0].instrument.kind, "sampler");
        assert_eq!(q.channels[0].instrument.sample.as_deref(), Some("/samples/piano.wav"));
        assert_eq!(q.channels.len(), 2);
        assert_eq!(p.layer_export_notes().len(), 2);
    }

    #[test]
    fn a_project_comes_back_through_the_reader() {
        let mut p = crate::demo_project();
        p.name = "Nuit étoilée".into();
        p.bpm = 128.5;
        p.tracks[3].name = "Drums bus".into();
        p.tracks[3].gain = 0.5;
        p.tracks[3].pan = -0.25;
        p.tracks[3].color = 0x3fd45a;
        p.clips.push(ClipDef { pattern: 0, lane: 7, start: TICKS_PER_BAR * 9, len: TICKS_PER_BAR, offset: TICKS_PER_STEP * 2, muted: false });
        let bytes = export_flp(&p, &|_| None);
        let c = parse_container(&bytes).unwrap();
        assert_eq!(c.unread, 0, "every event reads back");
        assert_eq!(c.header.ppq as u32, PPQ);

        let (q, report) = import_flp(&bytes, "x", &|raw| Some(raw.to_string())).unwrap();
        assert_eq!(q.name, p.name);
        assert!((q.bpm - 128.5).abs() < 1e-3);
        // Channels: names, mixer tracks, levels.
        assert_eq!(q.channels.len(), p.channels.len(), "{report:?}");
        for (a, b) in p.channels.iter().zip(&q.channels) {
            assert_eq!(a.name, b.name);
            assert_eq!(a.track, b.track);
            assert!((a.gain.min(1.25) - b.gain).abs() < 1e-3, "{} {}", a.gain, b.gain);
            assert!((a.pan - b.pan).abs() < 1e-3);
            assert_eq!(a.muted, b.muted);
            assert_eq!(a.instrument.kind == "sampler", b.instrument.kind == "sampler", "{}", a.name);
            if a.instrument.kind == "sampler" {
                assert_eq!(a.instrument.sample, b.instrument.sample);
            }
        }
        // Patterns: every note at its place.
        let with_notes: Vec<&PatternDef> = p.patterns.iter().collect();
        assert_eq!(q.patterns.len(), with_notes.len());
        for (a, b) in with_notes.iter().zip(&q.patterns) {
            assert_eq!(a.name, b.name);
            assert_eq!(a.len, b.len);
            for ch in 0..p.channels.len() {
                let key = |n: &Note| (n.start, n.len, n.key, (n.vel * 128.0).round() as u32);
                let want: Vec<_> = a.notes.get(ch).map_or(Vec::new(), |v| v.iter().filter(|n| !n.slide).map(key).collect());
                let got: Vec<_> = b.notes.get(ch).map_or(Vec::new(), |v| v.iter().map(key).collect());
                assert_eq!(want, got, "pattern {} channel {ch}", a.name);
            }
        }
        // Playlist clips, offsets included.
        let clips = |p: &Project| {
            let mut v: Vec<_> = p.clips.iter().map(|c| (c.start, c.pattern, c.lane, c.len, c.offset)).collect();
            v.sort();
            v
        };
        assert_eq!(clips(&q), clips(&p));
        // Mixer.
        assert_eq!(q.tracks[3].name, "Drums bus");
        assert!((q.tracks[3].gain - 0.5).abs() < 1e-3 && (q.tracks[3].pan + 0.25).abs() < 1e-3);
        assert_eq!(q.tracks[3].color, 0x3fd45a);
        for (i, t) in p.tracks.iter().enumerate().skip(1).take(q.tracks.len() - 1) {
            assert_eq!(q.tracks[i].output, t.output, "track {i}");
            assert_eq!(q.tracks[i].muted, t.muted, "track {i}");
        }
    }

    #[test]
    fn audio_clips_get_a_sampler_channel_each() {
        let mut p = Project::empty();
        p.name = "Audio".into();
        p.audio_clips.push(AudioClipDef { path: "/a/Vocal take.wav".into(), lane: 2, track: 4, start: 96, len: 384, offset: 0.5, gain: 1.0, ..Default::default() });
        p.audio_clips.push(AudioClipDef { path: "/a/Vocal take.wav".into(), lane: 3, track: 4, start: 960, len: 192, gain: 1.0, ..Default::default() });
        let bytes = export_flp(&p, &|raw| Some(format!("C:{raw}")));
        let (q, _) = import_flp(&bytes, "x", &|raw| Some(raw.to_string())).unwrap();
        assert_eq!(q.audio_clips.len(), 2);
        let a = &q.audio_clips[0];
        assert_eq!((a.path.as_str(), a.lane, a.track, a.start, a.len), ("C:/a/Vocal take.wav", 2, 4, 96, 384));
        assert!((a.offset - 0.5).abs() < 1e-3);
        assert_eq!(q.audio_clips[1].offset, 0.0);
    }

    #[test]
    fn an_empty_project_is_still_a_file() {
        let bytes = export_flp(&Project::empty(), &|_| None);
        let (q, _) = import_flp(&bytes, "x", &|_| None).unwrap();
        assert!(q.channels.is_empty() && q.clips.is_empty());
    }
}
