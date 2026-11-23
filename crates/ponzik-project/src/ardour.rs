//! Ardour sessions (`.ardour`, Ardour 5 to 8).
//!
//! A session is XML next to its media: audio files in
//! `interchange/<session>/audiofiles`, MIDI regions as Standard MIDI Files in
//! `interchange/<session>/midifiles`. Ardour 5 and 6 write positions in
//! samples; Ardour 7 and 8 write time positions as `a<superclock>` (audio
//! time) or `b<ticks>` (music time, 1920 ticks per quarter note).
//!
//! Import reads the tempo map (tempo changes, ramps, meters), the markers,
//! ranges, sections and the loop, every track and bus in the editor's order
//! (name, colour, gain, pan, mute; tracks routed into a bus by their output
//! ports, internal sends), the closest Ponzik instrument from a MIDI track's
//! first plugin, MIDI regions (their notes from the session's MIDI files)
//! as patterns in the playlist and audio regions as audio clips (file,
//! place, length, start in the file, gain, fades). Plugins, automation and
//! VCAs stay behind.

use std::path::{Path, PathBuf};

use ponzik_engine::mixer::MASTER;
use ponzik_engine::{Note, PPQ, TempoMap};
use ponzik_i18n::{tr, tr_fmt};

use crate::als::{Element, parse_xml};
use crate::foreign::{Build, MAX_FILE, finite, instrument_for_name, route, to_tick};
use crate::{AudioClipDef, ImportReport, InstrumentDef, Project};

/// Ardour 7's superclock rate, when the session does not say.
const SUPERCLOCK: f64 = 282_240_000.0;
/// Ardour's music-time ticks per quarter note.
const BEAT_TICKS: f64 = 1920.0;

fn num(e: &Element, key: &str) -> Option<f64> {
    e.attr(key)?.trim().parse().ok().filter(|v: &f64| v.is_finite())
}

/// A time position of the session.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Pos {
    Secs(f64),
    Beats(f64),
}

/// Reads the clocks of a session.
struct Clock {
    rate: f64,
    superclock: f64,
}

impl Clock {
    fn pos(&self, s: &str) -> Option<Pos> {
        let s = s.trim();
        let v = |t: &str| t.parse::<f64>().ok().filter(|v| v.is_finite());
        if let Some(a) = s.strip_prefix('a') {
            Some(Pos::Secs(v(a)? / self.superclock))
        } else if let Some(b) = s.strip_prefix('b') {
            Some(Pos::Beats(v(b)? / BEAT_TICKS))
        } else {
            Some(Pos::Secs(v(s)? / self.rate))
        }
    }

    fn attr(&self, e: &Element, key: &str) -> Option<Pos> {
        self.pos(e.attr(key)?)
    }
}

/// Ticks of a position on the tempo map.
fn tick(map: &TempoMap, p: Pos) -> u32 {
    match p {
        Pos::Secs(s) => to_tick(map.tick_at_secs(s.max(0.0))),
        Pos::Beats(b) => to_tick(b * PPQ as f64),
    }
}

/// Ticks of a duration `len` from `start`.
fn span(map: &TempoMap, start: Pos, len: Pos) -> u32 {
    let from = tick(map, start);
    let to = match (start, len) {
        (Pos::Secs(s), Pos::Secs(l)) => tick(map, Pos::Secs(s + l)),
        (_, Pos::Beats(l)) => to_tick(from as f64 + l * PPQ as f64),
        (Pos::Beats(_), Pos::Secs(l)) => to_tick(map.tick_at_secs(map.secs_at(from as f64) + l)),
    };
    to.saturating_sub(from).max(1)
}

/// A duration in seconds (music time at the tempo at `at`).
fn secs(map: &TempoMap, at: u32, p: Pos) -> f64 {
    match p {
        Pos::Secs(s) => s,
        Pos::Beats(b) => b * 60.0 / map.bpm_at(at as f64),
    }
}

/// A duration in beats (audio time at the tempo at `at`).
fn beats(map: &TempoMap, at: u32, p: Pos) -> f64 {
    match p {
        Pos::Beats(b) => b,
        Pos::Secs(s) => s * map.bpm_at(at as f64) / 60.0,
    }
}

/// The value of the control `name` inside `e` (searched in depth).
fn control(e: &Element, name: &str) -> Option<f64> {
    let mut found = Vec::new();
    e.find_all("Controllable", &mut found);
    found.into_iter().find(|c| c.attr("name") == Some(name)).and_then(|c| num(c, "value"))
}

/// Plugin processors (not Ardour's own amp, meter, sends…).
fn plugins(route: &Element) -> impl Iterator<Item = &Element> {
    route.children_named("Processor").filter(|p| {
        matches!(
            p.attr("type").unwrap_or(""),
            "lv2" | "ladspa" | "lua" | "luaproc" | "vst" | "windows-vst" | "lxvst" | "mac-vst" | "vst3" | "audiounit"
        )
    })
}

/// The length of a fade: the time of the last point of its curve.
fn fade(clock: &Clock, e: Option<&Element>) -> Option<Pos> {
    let mut lists = Vec::new();
    e?.find_all("events", &mut lists);
    let text = &lists.first()?.text;
    let times: Vec<Pos> = text.split_whitespace().step_by(2).filter_map(|t| clock.pos(t)).collect();
    times.last().copied()
}

/// Notes of a MIDI file, merged across its tracks.
fn smf_notes(bytes: &[u8]) -> Option<Vec<Note>> {
    let file = crate::read_smf(bytes).ok()?;
    let mut notes: Vec<Note> = file.tracks.into_iter().flat_map(|t| t.notes).collect();
    notes.sort_by_key(|n| (n.start, n.key));
    Some(notes)
}

/// Converts an Ardour session into a new Ponzik project. `dir` is the
/// session's folder (its media are there); `resolve` turns a media path into
/// a readable path.
pub fn import_ardour(
    bytes: &[u8],
    name: &str,
    dir: Option<&Path>,
    resolve: &dyn Fn(&str) -> Option<String>,
) -> Result<(Project, ImportReport), String> {
    if bytes.len() > MAX_FILE {
        return Err(tr("Not an Ardour session").into());
    }
    let text = String::from_utf8_lossy(bytes);
    let root = parse_xml(&text)?;
    if root.name != "Session" {
        return Err(tr("Not an Ardour session").into());
    }
    let session = root.attr("name").filter(|s| !s.is_empty()).unwrap_or(name).to_string();
    let mut b = Build::new("Ardour", &session);
    let created = root.child("ProgramVersion").and_then(|v| v.attr("modified-with").or(v.attr("created-with")));
    if let Some(v) = created {
        b.report.mapped.push(tr_fmt!("Saved by {}", v));
    }
    let tempo_map = root.child("TempoMap");
    let clock = Clock {
        rate: num(&root, "sample-rate").filter(|r| *r >= 1000.0).unwrap_or(48_000.0),
        superclock: tempo_map
            .and_then(|t| num(t, "superclocks-per-second"))
            .or_else(|| num(&root, "superclocks-per-second"))
            .filter(|r| *r >= 1.0)
            .unwrap_or(SUPERCLOCK),
    };

    // Tempo map: (beats, quarter-note BPM, ramped to the next one).
    let mut tempos: Vec<(f64, f64, bool)> = Vec::new();
    let mut meters: Vec<(f64, u32, u32)> = Vec::new();
    if let Some(tm) = tempo_map {
        let quarters = |e: &Element| -> Option<f64> {
            if let Some(q) = e.attr("quarters") {
                let (beats, ticks) = q.split_once(':').unwrap_or((q, "0"));
                let (b, t) = (beats.trim().parse::<f64>().ok()?, ticks.trim().parse::<f64>().unwrap_or(0.0));
                return Some(b + t / BEAT_TICKS).filter(|v| v.is_finite());
            }
            num(e, "pulse").map(|p| p * 4.0)
        };
        fn list<'a>(tm: &'a Element, name: &str, inner: &'a str) -> Vec<&'a Element> {
            let mut v: Vec<&Element> = tm.children_named(inner).collect();
            if let Some(group) = tm.child(name) {
                v.extend(group.children_named(inner));
            }
            v
        }
        for t in list(tm, "Tempos", "Tempo") {
            let note = num(t, "note-type").filter(|n| *n > 0.0).unwrap_or(4.0);
            let Some(npm) = num(t, "npm").or_else(|| num(t, "beats-per-minute")) else { continue };
            let ramped = t.attr("type").or(t.attr("tempo-type")).is_some_and(|k| k.eq_ignore_ascii_case("ramped"))
                || num(t, "enpm").or_else(|| num(t, "end-beats-per-minute")).is_some_and(|e| (e - npm).abs() > 1e-6);
            tempos.push((quarters(t).unwrap_or(0.0).max(0.0), npm * 4.0 / note, ramped));
        }
        for m in list(tm, "Meters", "Meter") {
            let div = num(m, "divisions-per-bar").unwrap_or(4.0);
            let note = num(m, "note-value").or_else(|| num(m, "note-type")).unwrap_or(4.0);
            meters.push((quarters(m).unwrap_or(0.0).max(0.0), div as u32, note as u32));
        }
    }
    tempos.sort_by(|a, b| a.0.total_cmp(&b.0));
    let first = tempos.first().map_or(120.0, |t| t.1);
    let changes: Vec<(u32, f64, bool)> =
        tempos.windows(2).map(|w| (to_tick(w[1].0 * PPQ as f64), w[1].1, w[0].2)).collect();
    b.tempo(first, &changes);
    for (q, div, note) in meters {
        b.signature(to_tick(q * PPQ as f64), div, note);
    }
    let map = b.p.tempo_map();

    // Locations.
    let (mut ranges, mut cues) = (0, 0);
    if let Some(locs) = root.child("Locations") {
        for l in locs.children_named("Location") {
            let flags = l.attr("flags").unwrap_or("");
            let has = |f: &str| flags.split(',').any(|x| x.trim() == f);
            let Some(start) = clock.attr(l, "start") else { continue };
            let label = l.attr("name").unwrap_or("");
            if has("IsAutoLoop") {
                if let Some(end) = clock.attr(l, "end") {
                    let (s, e) = (tick(&map, start), tick(&map, end));
                    if e > s {
                        b.p.loop_range = Some((s, e));
                        b.report.mapped.push(tr("Loop range").into());
                    }
                }
            } else if !(has("IsSessionRange") || has("IsAutoPunch") || has("IsXrun") || has("IsSkip"))
                && (has("IsMark") || has("IsRangeMarker") || has("IsSection") || has("IsCueMarker") || has("IsCDMarker"))
            {
                if has("IsRangeMarker") {
                    ranges += 1;
                }
                if has("IsCueMarker") {
                    cues += 1;
                }
                b.marker(tick(&map, start), label, 0);
            }
        }
    }
    if ranges > 0 {
        b.report.approximated.push(tr_fmt!("{} ranges became markers at their start", ranges));
    }
    if cues > 0 {
        b.report.approximated.push(tr_fmt!("{} cue markers became plain markers", cues));
    }

    // Media.
    let sources: Vec<&Element> = root.child("Sources").map_or_else(Vec::new, |s| s.children_named("Source").collect());
    let interchange: Vec<PathBuf> = {
        let mut v = Vec::new();
        if let Some(d) = dir {
            v.push(d.join("interchange").join(&session));
            if let Ok(rd) = std::fs::read_dir(d.join("interchange")) {
                v.extend(rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()));
            }
        }
        v.dedup();
        v
    };
    // The path of a source file: as written when absolute, else in the session's media folders.
    let source_path = |id: &str| -> Option<(String, bool)> {
        let s = sources.iter().find(|s| s.attr("id") == Some(id))?;
        let file = s.attr("name").filter(|n| !n.is_empty())?;
        let midi = s.attr("type") == Some("midi");
        if file.starts_with('/') || file.contains(":\\") || file.contains(":/") {
            return Some((file.to_string(), midi));
        }
        let sub = if midi { "midifiles" } else { "audiofiles" };
        let found = interchange.iter().map(|d| d.join(sub).join(file)).find(|p| p.is_file());
        let path = found.unwrap_or_else(|| interchange.first().map_or_else(|| PathBuf::from(file), |d| d.join(sub).join(file)));
        Some((path.to_string_lossy().into_owned(), midi))
    };
    let playlists: Vec<&Element> = root.child("Playlists").map_or_else(Vec::new, |p| p.children_named("Playlist").collect());

    // Routes, in the editor's order.
    let mut routes: Vec<&Element> = root.child("Routes").map_or_else(Vec::new, |r| r.children_named("Route").collect());
    let flags = |r: &Element| r.child("PresentationInfo").and_then(|p| p.attr("flags")).unwrap_or("").to_string();
    let order = |r: &Element| r.child("PresentationInfo").and_then(|p| num(p, "order")).unwrap_or(f64::MAX);
    routes.sort_by(|a, c| order(a).total_cmp(&order(c)));
    let mut slots: Vec<(String, String, usize)> = Vec::new(); // (route id, name, mixer track)
    let (mut effects, mut stereo, mut missing_midi, mut lane) = (0, 0, 0, 0);
    for r in &routes {
        let f = flags(r);
        let label = r.attr("name").unwrap_or("").to_string();
        let gain = r.children_named("Processor").find(|p| p.attr("type") == Some("amp")).and_then(|p| control(p, "gaincontrol"));
        if f.contains("MasterOut") {
            b.p.tracks[MASTER].gain = finite(gain.unwrap_or(1.0), 1.0).clamp(0.0, 4.0);
            continue;
        }
        if f.contains("MonitorOut") || f.contains("FoldbackBus") {
            continue;
        }
        let color = r.child("PresentationInfo").and_then(|p| num(p, "color")).map_or(0, |c| (c as u64 >> 8) as u32 & 0xff_ffff);
        // Azimuth 0 is hard left, 1 hard right.
        let pan = r.child("Pannable").and_then(|p| control(p, "pan-azimuth")).map_or(0.0, |a| a * 2.0 - 1.0);
        let muted = r.children_named("Controllable").find(|c| c.attr("name") == Some("mute")).and_then(|c| num(c, "value")).unwrap_or(0.0) > 0.5
            || r.child("MuteMaster").is_some_and(|m| m.attr("muted") == Some("1") || m.attr("muted") == Some("yes"));
        let slot = b.track(&label, color, gain.unwrap_or(1.0) as f32, pan as f32, muted);
        slots.push((r.attr("id").unwrap_or("").to_string(), label.clone(), slot));
        let is_midi = f.contains("MidiTrack") || (r.attr("default-type") == Some("midi") && !f.contains("Bus"));
        let is_track = f.contains("AudioTrack") || f.contains("MidiTrack") || r.child("Diskstream").is_some()
            || r.attr("audio-playlist").is_some() || r.attr("midi-playlist").is_some();
        let mut plugs = plugins(r);
        let instrument = if is_midi { plugs.next() } else { None };
        effects += plugs.count();
        if !is_track {
            continue;
        }
        let this_lane = lane;
        lane += 1;
        let channel = is_midi.then(|| {
            let (kind, from) = match instrument.and_then(|p| p.attr("name")) {
                Some(pn) => (instrument_for_name(pn), Some(pn.to_string())),
                None => (instrument_for_name(&label), None),
            };
            if let Some(pn) = from {
                b.report.approximated.push(tr_fmt!(
                    "'{}': {} replaced by {} (settings not imported)",
                    label,
                    pn,
                    crate::foreign::instrument_name(kind)
                ));
            }
            b.channel(&label, InstrumentDef::new(kind), slot, color)
        });
        // The track's playlist: by id (Ardour 6+), by name (Ardour 5), or the one made for it.
        let id = r.attr("id").unwrap_or("");
        let pl_ref = r.attr("audio-playlist").or(r.attr("midi-playlist")).or_else(|| r.child("Diskstream").and_then(|d| d.attr("playlist")));
        let playlist = pl_ref
            .and_then(|v| playlists.iter().find(|p| p.attr("id") == Some(v)).or_else(|| playlists.iter().find(|p| p.attr("name") == Some(v))))
            .or_else(|| playlists.iter().find(|p| p.attr("orig-track-id") == Some(id)));
        let Some(pl) = playlist else { continue };
        for reg in pl.children_named("Region") {
            let (Some(pos), Some(len)) = (clock.attr(reg, "position"), clock.attr(reg, "length")) else { continue };
            let start = tick(&map, pos);
            let clip_len = span(&map, pos, len);
            let muted = reg.attr("muted") == Some("1");
            let Some((path, midi)) = reg.attr("source-0").and_then(source_path) else { continue };
            if midi {
                let Some(ch) = channel else { continue };
                let notes = std::fs::read(&path).ok().and_then(|bytes| smf_notes(&bytes));
                let Some(notes) = notes else {
                    missing_midi += 1;
                    b.report.offline.push(tr_fmt!("{} not found on this computer", path));
                    continue;
                };
                let offset_beats = match num(reg, "start-beats") {
                    Some(sb) => sb,
                    None => clock.attr(reg, "start").map_or(0.0, |s| beats(&map, start, s)),
                };
                let offset = to_tick(offset_beats * PPQ as f64);
                let end = notes.iter().map(|n| n.start + n.len).max().unwrap_or(0);
                let label = reg.attr("name").unwrap_or(&label).to_string();
                let pattern = b.pattern(ch, notes, end.max(offset + clip_len), &label, 0);
                b.clip(pattern, this_lane, start, clip_len, offset, muted);
            } else {
                if reg.attr("source-1").is_some() {
                    stereo += 1;
                }
                let path = b.locate(&path, resolve, None);
                let offset = clock.attr(reg, "start").map_or(0.0, |s| secs(&map, start, s));
                let fade_len = |key: &str, active: &str| -> f32 {
                    if reg.attr(active) == Some("0") {
                        return 0.0;
                    }
                    fade(&clock, reg.child(key)).map_or(64.0 / clock.rate, |p| secs(&map, start, p)) as f32
                };
                b.audio(AudioClipDef {
                    path,
                    lane: this_lane,
                    track: slot,
                    start,
                    len: clip_len,
                    offset,
                    gain: num(reg, "scale-amplitude").unwrap_or(1.0) as f32,
                    fade_in: fade_len("FadeIn", "fade-in-active"),
                    fade_out: fade_len("FadeOut", "fade-out-active"),
                    muted,
                    ..Default::default()
                });
            }
        }
    }
    // Routing: outputs connected to a bus's inputs, internal sends.
    let (mut routed, mut sends) = (0, 0);
    for r in &routes {
        let Some(&(_, _, from)) = slots.iter().find(|s| Some(s.0.as_str()) == r.attr("id")) else { continue };
        let outs = r.children_named("IO").filter(|io| io.attr("direction") == Some("Output"));
        let mut target = None;
        for io in outs {
            let mut conns = Vec::new();
            io.find_all("Connection", &mut conns);
            for c in conns {
                let other = c.attr("other").unwrap_or("");
                let dest = other.rsplit_once('/').map_or(other, |(n, _)| n);
                if let Some(s) = slots.iter().find(|s| s.1 == dest && s.2 != from) {
                    target = target.or(Some(s.2));
                }
            }
        }
        if let Some(to) = target {
            if route(&mut b.p, from, to) {
                routed += 1;
            }
        }
        for p in r.children_named("Processor").filter(|p| p.attr("type") == Some("intsend")) {
            let Some(&(_, _, to)) = slots.iter().find(|s| Some(s.0.as_str()) == p.attr("target")) else { continue };
            let level = control(p, "gaincontrol").unwrap_or(1.0) as f32;
            if let Some(slot) = b.p.tracks[from].sends.iter_mut().find(|s| s.0 == MASTER && s.1 == 0.0) {
                *slot = (to, finite(level, 1.0).clamp(0.0, 4.0));
                sends += 1;
            }
        }
    }
    if routed > 0 {
        b.report.mapped.push(tr_fmt!("{} tracks routed into a bus", routed));
    }
    if sends > 0 {
        b.report.mapped.push(tr_fmt!("{} sends", sends));
    }
    if stereo > 0 {
        b.report.approximated.push(tr_fmt!("{} regions of two mono files play their first channel only", stereo));
    }
    if missing_midi > 0 {
        b.report.skipped.push(tr_fmt!("{} MIDI regions whose file is missing", missing_midi));
    }
    if effects > 0 {
        b.report.skipped.push(tr_fmt!("{} effect plugins (not converted)", effects));
    }
    let vcas = root.child("VCAManager").map_or(0, |v| v.children_named("VCA").count());
    if vcas > 0 {
        b.report.skipped.push(tr_fmt!("{} VCAs", vcas));
    }
    b.report.skipped.push(tr("Automation").into());
    Ok(b.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ponzik_engine::TimeSig;

    /// A one-track SMF (480 PPQ): C4 for a beat, then E4 for a beat.
    fn smf() -> Vec<u8> {
        let track: Vec<u8> = vec![
            0x00, 0x90, 60, 100, 0x83, 0x60, 0x80, 60, 0, 0x00, 0x90, 64, 80, 0x83, 0x60, 0x80, 64, 0, 0x00, 0xff, 0x2f, 0x00,
        ];
        let mut out = b"MThd".to_vec();
        out.extend_from_slice(&6u32.to_be_bytes());
        out.extend_from_slice(&[0, 0, 0, 1, 0x01, 0xe0]);
        out.extend_from_slice(b"MTrk");
        out.extend_from_slice(&(track.len() as u32).to_be_bytes());
        out.extend_from_slice(&track);
        out
    }

    fn session_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ponzik-ardour-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("interchange/Demo/midifiles")).unwrap();
        std::fs::create_dir_all(dir.join("interchange/Demo/audiofiles")).unwrap();
        std::fs::write(dir.join("interchange/Demo/midifiles/Synth-1.mid"), smf()).unwrap();
        dir
    }

    /// An Ardour 8 session: superclock positions, a ramp, a meter change, a
    /// bus, a send, a MIDI track with a synth and an audio track.
    const ARDOUR8: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<Session version="7003" name="Demo" sample-rate="48000" end-is-free="0">
  <ProgramVersion created-with="Ardour 8.4" modified-with="Ardour 8.6"/>
  <Sources>
    <Source name="Vox-1.wav" type="audio" flags="" id="101" captured-for="Vox" channel="0" origin=""/>
    <Source name="Vox-1R.wav" type="audio" flags="" id="102" captured-for="Vox" channel="1" origin=""/>
    <Source name="Synth-1.mid" type="midi" flags="" id="103" origin=""/>
  </Sources>
  <Locations>
    <Location id="1" name="session" start="a0" end="a2822400000" flags="IsSessionRange"/>
    <Location id="2" name="Loop" start="a282240000" end="a564480000" flags="IsAutoLoop,IsHidden"/>
    <Location id="3" name="Chorus" start="a564480000" end="a564480000" flags="IsMark"/>
    <Location id="4" name="Bridge" start="a846720000" end="a1128960000" flags="IsRangeMarker,IsSection"/>
  </Locations>
  <Routes>
    <Route version="7003" id="10" name="Master" default-type="audio">
      <PresentationInfo order="0" flags="MasterOut,OrderSet" color="0"/>
      <Processor id="11" name="Amp" type="amp"><Controllable name="gaincontrol" id="12" value="0.5"/></Processor>
    </Route>
    <Route version="7003" id="40" name="Bus 1" default-type="audio">
      <PresentationInfo order="3" flags="AudioBus,OrderSet" color="4278190335"/>
      <Processor id="41" name="Amp" type="amp"><Controllable name="gaincontrol" value="1"/></Processor>
      <Processor id="42" name="ACE Delay" type="lv2"/>
    </Route>
    <Route version="7003" id="20" name="Synth" default-type="midi" midi-playlist="200">
      <PresentationInfo order="1" flags="MidiTrack,OrderSet" color="16711935"/>
      <Controllable name="mute" id="21" value="1"/>
      <IO name="Synth" direction="Output" default-type="audio"><Port type="audio" name="Synth/audio_out 1"><Connection other="Bus 1/audio_in 1"/></Port></IO>
      <Pannable><Controllable name="pan-azimuth" value="0.25"/></Pannable>
      <Processor id="22" name="Amp" type="amp"><Controllable name="gaincontrol" value="0.7"/></Processor>
      <Processor id="23" name="Dexed" type="lv2"/>
      <Processor id="24" name="ACE Compressor" type="lv2"/>
    </Route>
    <Route version="7003" id="30" name="Vox" default-type="audio" audio-playlist="300">
      <PresentationInfo order="2" flags="AudioTrack,OrderSet" color="0"/>
      <IO name="Vox" direction="Output"><Port type="audio" name="Vox/audio_out 1"><Connection other="Master/audio_in 1"/></Port></IO>
      <Processor id="31" name="Amp" type="amp"><Controllable name="gaincontrol" value="1"/></Processor>
      <Processor id="32" name="Bus 1" type="intsend" target="40"><Processor type="amp"><Controllable name="gaincontrol" value="0.3"/></Processor></Processor>
    </Route>
  </Routes>
  <Playlists>
    <Playlist id="200" name="Synth" type="midi" orig-track-id="20">
      <Region name="Synth-1" muted="0" position="b3840" length="b7680" start="b1920" source-0="103"/>
    </Playlist>
    <Playlist id="300" name="Vox" type="audio" orig-track-id="30">
      <Region name="Vox-1" muted="1" position="a282240000" length="a564480000" start="a141120000" scale-amplitude="0.5" fade-in-active="1" fade-out-active="0" source-0="101" source-1="102">
        <FadeIn><AutomationList id="1" interpolation-style="Linear"><events>a0 0
a28224000 1
</events></AutomationList></FadeIn>
      </Region>
    </Playlist>
  </Playlists>
  <TempoMap superclocks-per-second="282240000">
    <Tempos>
      <Tempo npm="120" enpm="120" note-type="4" type="Ramped" active="1" sclock="0" quarters="0:0" bbt="1|1|0"/>
      <Tempo npm="140" enpm="140" note-type="4" type="Constant" active="1" sclock="0" quarters="16:0" bbt="5|1|0"/>
    </Tempos>
    <Meters>
      <Meter sclock="0" quarters="0:0" bbt="1|1|0" note-value="4" divisions-per-bar="4"/>
      <Meter sclock="0" quarters="16:0" bbt="5|1|0" note-value="8" divisions-per-bar="6"/>
    </Meters>
  </TempoMap>
  <VCAManager><VCA name="VCA 1"/></VCAManager>
</Session>
"#;

    #[test]
    fn an_ardour_8_session_imports() {
        let dir = session_dir("8");
        let (p, report) = import_ardour(ARDOUR8.as_bytes(), "x", Some(&dir), &|raw| Some(raw.to_string())).unwrap();
        assert_eq!(report.source, "Ardour");
        assert_eq!(p.name, "Demo");
        assert_eq!(p.bpm, 120.0);
        assert_eq!(p.tempo_changes.len(), 1);
        assert_eq!((p.tempo_changes[0].tick, p.tempo_changes[0].bpm, p.tempo_changes[0].ramp), (16 * PPQ, 140.0, true));
        assert_eq!(p.signatures, vec![TimeSig { tick: 16 * PPQ, num: 6, den: 8 }]);
        assert_eq!(p.tracks[MASTER].gain, 0.5);
        // 1 s and 2 s into the ramp: before beat 16, so the ramp counts.
        let map = p.tempo_map();
        let loop_range = p.loop_range.unwrap();
        assert_eq!(loop_range.0, map.tick_at_secs(1.0).round() as u32);
        let marks: Vec<&str> = p.markers.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(marks, ["Chorus", "Bridge"]);
        // Tracks in the editor's order: Synth, Vox, Bus 1.
        let names: Vec<&str> = p.tracks[1..4].iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["Synth", "Vox", "Bus 1"]);
        let synth = &p.tracks[1];
        assert!((synth.gain - 0.7).abs() < 1e-6 && (synth.pan + 0.5).abs() < 1e-6 && synth.muted);
        assert_eq!((synth.color, synth.output), (0x00ff00, 3));
        assert_eq!(p.tracks[3].color, 0xff0000);
        assert_eq!(p.tracks[2].output, MASTER);
        assert_eq!(p.tracks[2].sends[0], (3, 0.3));
        // The MIDI region: 4 beats from beat 2, 1 beat into its file.
        assert_eq!(p.channels.len(), 1);
        assert_eq!((p.channels[0].instrument.kind.as_str(), p.channels[0].track), ("fm", 1));
        assert_eq!(p.clips.len(), 1);
        let c = p.clips[0];
        assert_eq!((c.start, c.len, c.offset), (2 * PPQ, 4 * PPQ, PPQ));
        let pat = &p.patterns[c.pattern];
        assert_eq!(pat.notes[0].iter().map(|n| (n.start, n.len, n.key)).collect::<Vec<_>>(), [(0, PPQ, 60), (PPQ, PPQ, 64)]);
        assert_eq!(pat.len, 5 * PPQ);
        // The audio region: 1 s in, 2 s long, 0.5 s into the file.
        assert_eq!(p.audio_clips.len(), 1);
        let a = &p.audio_clips[0];
        assert!(a.path.ends_with("interchange/Demo/audiofiles/Vox-1.wav"), "{}", a.path);
        assert_eq!(a.start, map.tick_at_secs(1.0).round() as u32);
        assert_eq!(a.len, map.tick_at_secs(3.0).round() as u32 - a.start);
        assert!((a.offset - 0.5).abs() < 1e-9 && (a.gain - 0.5).abs() < 1e-6 && (a.fade_in - 0.1).abs() < 1e-6);
        assert!(a.muted && a.fade_out == 0.0);
        assert!(report.approximated.iter().any(|s| s.contains("two mono files")));
        assert!(report.skipped.iter().any(|s| s.contains("2 effect plugins")));
        assert!(report.skipped.iter().any(|s| s.contains("1 VCAs")));
        assert!(report.approximated.iter().any(|s| s.contains("Dexed")));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// An Ardour 6 session: positions in samples, the old tempo map.
    #[test]
    fn an_ardour_6_session_imports() {
        let dir = session_dir("6");
        let xml = r#"<Session version="6000" name="Demo" sample-rate="44100">
  <Sources><Source name="Synth-1.mid" type="midi" id="9"/><Source name="/abs/loop.flac" type="audio" id="8"/></Sources>
  <Locations><Location name="m" start="88200" end="88200" flags="IsMark"/></Locations>
  <Routes>
    <Route id="1" name="Keys" default-type="midi" midi-playlist="5"><PresentationInfo order="0" flags="MidiTrack"/>
      <MuteMaster mute-point="PreFader,PostFader" muted="1"/></Route>
    <Route id="2" name="Loop" default-type="audio" audio-playlist="6"><PresentationInfo order="1" flags="AudioTrack"/></Route>
  </Routes>
  <Playlists>
    <Playlist id="5" name="Keys"><Region name="r" position="44100" length="88200" start="0" start-beats="0" source-0="9"/></Playlist>
    <Playlist id="6" name="Loop"><Region name="l" position="0" length="22050" start="4410" source-0="8"/></Playlist>
  </Playlists>
  <TempoMap><Tempo pulse="0" frame="0" beats-per-minute="60" note-type="4" tempo-type="Constant"/>
    <Meter pulse="0" frame="0" divisions-per-bar="3" note-type="4"/></TempoMap>
</Session>"#;
        let (p, _) = import_ardour(xml.as_bytes(), "x", Some(&dir), &|_| None).unwrap();
        assert_eq!(p.bpm, 60.0);
        assert_eq!(p.signatures, vec![TimeSig { tick: 0, num: 3, den: 4 }]);
        assert_eq!(p.markers[0].tick, 2 * PPQ);
        assert!(p.tracks[1].muted);
        assert_eq!(p.channels[0].instrument.kind, "epiano");
        assert_eq!((p.clips[0].start, p.clips[0].len), (PPQ, 2 * PPQ));
        let a = &p.audio_clips[0];
        assert_eq!((a.path.as_str(), a.start, a.len), ("/abs/loop.flac", 0, PPQ / 2));
        assert!((a.offset - 0.1).abs() < 1e-9);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn junk_is_an_error_never_a_panic() {
        assert!(import_ardour(b"", "x", None, &|_| None).is_err());
        assert!(import_ardour(b"<Other/>", "x", None, &|_| None).is_err());
        let (p, _) = import_ardour(b"<Session/>", "x", None, &|_| None).unwrap();
        assert!(p.channels.is_empty());
        for cut in (0..ARDOUR8.len()).step_by(13) {
            let _ = import_ardour(&ARDOUR8.as_bytes()[..cut], "x", None, &|_| None);
        }
        let junk = r#"<Session sample-rate="0"><TempoMap superclocks-per-second="0"><Tempos><Tempo npm="nan" quarters="x:y"/><Tempo npm="1e30" quarters="1e300:0"/></Tempos></TempoMap>
          <Routes><Route id="1" midi-playlist="1" default-type="midi"><PresentationInfo color="-5" order="nan"/></Route></Routes>
          <Playlists><Playlist id="1"><Region position="a1e300" length="b-5" source-0="1"/></Playlist></Playlists>
          <Sources><Source id="1" name="../../../etc/passwd" type="midi"/></Sources></Session>"#;
        let (p, _) = import_ardour(junk.as_bytes(), "x", None, &|_| None).unwrap();
        assert!(p.bpm.is_finite());
    }
}
