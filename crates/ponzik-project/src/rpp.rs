//! REAPER projects (`.rpp`): import one, export an interchange copy.
//!
//! An RPP file is plain text: chunks open with `<NAME args` and close with
//! `>`, every other line is a keyword and its values (strings quoted with
//! `"`, `'` or a backtick). Positions are in seconds; MIDI events inside an
//! item are in ticks per quarter note (`HASDATA 1 960 QN`), each one a delta
//! from the previous.
//!
//! Import reads the tempo, the time signature, the tempo envelope (changes,
//! ramps, signature changes), markers and regions, the loop, and every track:
//! name, colour, volume, pan, mute, folders (children routed into their
//! folder track) and sends; the closest Ponzik instrument from the first
//! instrument plugin (or the track's name; ReaSamplOmatic5000 becomes a
//! sampler with its file); MIDI items as patterns placed in the playlist
//! (looped or not, with their start offset) and audio items (file, place,
//! length, start in the file, gain, fades, reversed sections). Effects,
//! automation envelopes and take FX stay behind, counted in the report.
//!
//! Export writes the tempo, tempo changes and time signatures, markers, the
//! loop, one track per channel with a MIDI item per playlist clip (patterns
//! unrolled) and one track per mixer track that plays audio clips.

use std::fmt::Write as _;

use ponzik_engine::mixer::MASTER;
use ponzik_engine::{Note, PPQ, TempoMap};
use ponzik_i18n::{tr, tr_fmt};

use crate::foreign::{Build, MAX_FILE, finite, instrument_for_name, route, to_tick};
use crate::{AudioClipDef, ClipDef, ImportReport, InstrumentDef, Project};

// --- Reading the text ---

/// A line of a chunk: its values, or a chunk inside it.
#[derive(Clone, Debug, PartialEq)]
enum Line {
    Values(Vec<String>),
    Chunk(Chunk),
}

/// A chunk: its header (`<NAME args`) and its lines.
#[derive(Clone, Debug, Default, PartialEq)]
struct Chunk {
    head: Vec<String>,
    body: Vec<Line>,
}

/// Deeper chunks are skipped (garbage protection).
const MAX_DEPTH: usize = 64;

impl Chunk {
    fn name(&self) -> &str {
        self.head.first().map_or("", String::as_str)
    }

    /// The values of the first line starting with `key` (the key left out).
    fn get(&self, key: &str) -> Option<&[String]> {
        self.body.iter().find_map(|l| match l {
            Line::Values(v) if v.first().is_some_and(|k| k == key) => Some(&v[1..]),
            _ => None,
        })
    }

    fn num(&self, key: &str, i: usize) -> Option<f64> {
        self.get(key)?.get(i)?.parse().ok().filter(|v: &f64| v.is_finite())
    }

    fn text(&self, key: &str) -> Option<&str> {
        self.get(key)?.first().map(String::as_str)
    }

    fn chunks<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Chunk> + 'a {
        self.body.iter().filter_map(move |l| match l {
            Line::Chunk(c) if c.name() == name => Some(c),
            _ => None,
        })
    }

    fn chunk(&self, name: &str) -> Option<&Chunk> {
        self.body.iter().find_map(|l| match l {
            Line::Chunk(c) if c.name() == name => Some(c),
            _ => None,
        })
    }
}

/// The values of a line: words, or strings in `"`, `'` or backticks.
fn tokens(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = line.trim_start();
    while !rest.is_empty() {
        let first = rest.chars().next().expect("not empty");
        if matches!(first, '"' | '\'' | '`') {
            let body = &rest[1..];
            let end = body.find(first).unwrap_or(body.len());
            out.push(body[..end].to_string());
            rest = body.get(end + 1..).unwrap_or("");
        } else {
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            out.push(rest[..end].to_string());
            rest = &rest[end..];
        }
        rest = rest.trim_start();
    }
    out
}

/// Parses an RPP file into its root chunk. Tolerant: unclosed chunks close at
/// the end, too deep ones are skipped.
fn parse(text: &str) -> Result<Chunk, String> {
    let mut stack: Vec<Chunk> = Vec::new();
    let mut root: Option<Chunk> = None;
    let mut skipped = 0usize;
    for raw in text.lines() {
        let line = raw.trim();
        if let Some(head) = line.strip_prefix('<') {
            if stack.len() >= MAX_DEPTH || skipped > 0 {
                skipped += 1;
                continue;
            }
            stack.push(Chunk { head: tokens(head), body: Vec::new() });
        } else if line == ">" {
            if skipped > 0 {
                skipped -= 1;
                continue;
            }
            if let Some(done) = stack.pop() {
                match stack.last_mut() {
                    Some(parent) => parent.body.push(Line::Chunk(done)),
                    None if root.is_none() => root = Some(done),
                    None => {}
                }
            }
        } else if !line.is_empty() && skipped == 0 {
            if let Some(top) = stack.last_mut() {
                top.body.push(Line::Values(tokens(line)));
            }
        }
    }
    while let Some(done) = stack.pop() {
        match stack.last_mut() {
            Some(parent) => parent.body.push(Line::Chunk(done)),
            None => root = root.or(Some(done)),
        }
    }
    root.ok_or_else(|| tr("Not a REAPER project").into())
}

// --- Colours ---

/// A colour of REAPER (the native Windows order, 0x00BBGGRR, with the
/// 0x1000000 flag when set) as RGB; 0 when none is set.
fn color_from(v: i64) -> u32 {
    if v & 0x100_0000 == 0 {
        return 0;
    }
    let v = v as u32;
    ((v & 0xff) << 16) | (v & 0xff00) | ((v >> 16) & 0xff)
}

fn color_to(rgb: u32) -> u32 {
    0x100_0000 | ((rgb & 0xff) << 16) | (rgb & 0xff00) | ((rgb >> 16) & 0xff)
}

// --- Import ---

/// One take of an item: its source and settings.
#[derive(Default)]
struct Take<'a> {
    name: Option<&'a str>,
    soffs: f64,
    vol: f64,
    rate: f64,
    source: Option<&'a Chunk>,
    selected: bool,
}

/// The takes of an item, the lines before the first `TAKE` being the first.
fn takes(item: &Chunk) -> Vec<Take<'_>> {
    let mut out = vec![Take { vol: 1.0, rate: 1.0, ..Default::default() }];
    for line in &item.body {
        match line {
            Line::Values(v) => {
                let num = |i: usize| v.get(i).and_then(|s| s.parse::<f64>().ok()).filter(|x| x.is_finite());
                let t = out.last_mut().expect("one take");
                match v[0].as_str() {
                    "TAKE" => out.push(Take { vol: 1.0, rate: 1.0, selected: v.iter().any(|s| s == "SEL"), ..Default::default() }),
                    "NAME" => t.name = v.get(1).map(String::as_str),
                    "SOFFS" => t.soffs = num(1).unwrap_or(0.0),
                    // Item volume, take pan, take volume (first take).
                    "VOLPAN" => t.vol = num(3).unwrap_or(1.0),
                    // Take pan, take volume.
                    "TAKEVOLPAN" => t.vol = num(2).unwrap_or(1.0),
                    "PLAYRATE" => t.rate = num(1).filter(|r| *r > 0.0).unwrap_or(1.0),
                    _ => {}
                }
            }
            Line::Chunk(c) if c.name() == "SOURCE" => {
                let t = out.last_mut().expect("one take");
                t.source = t.source.or(Some(c));
            }
            Line::Chunk(_) => {}
        }
    }
    out
}

/// Notes of a MIDI source in its own ticks, and where it ends:
/// (start, end, key, velocity, MIDI channel).
struct MidiSource {
    ppq: f64,
    notes: Vec<(u64, u64, u8, u8, u8)>,
    end: u64,
}

fn read_midi(src: &Chunk) -> MidiSource {
    let ppq = src.num("HASDATA", 1).filter(|v| *v >= 1.0).unwrap_or(960.0);
    let mut at = 0u64;
    let mut notes = Vec::new();
    // Held notes: (channel, key) → (start, velocity).
    let mut held: Vec<(u8, u8, u64, u8)> = Vec::new();
    for line in &src.body {
        let v = match line {
            Line::Values(v) => v,
            Line::Chunk(c) => &c.head,
        };
        let kind = v.first().map_or("", String::as_str);
        if !matches!(kind, "E" | "e" | "Em" | "em" | "X" | "x" | "Xm" | "xm") {
            continue;
        }
        let Some(delta) = v.get(1).and_then(|d| d.parse::<i64>().ok()) else { continue };
        at = at.saturating_add(delta.max(0) as u64).min(1 << 40);
        if !kind.starts_with(['E', 'e']) || kind.ends_with('m') {
            continue;
        }
        let byte = |i: usize| v.get(i).and_then(|h| u8::from_str_radix(h, 16).ok());
        let (Some(status), Some(key), Some(vel)) = (byte(2), byte(3), byte(4)) else { continue };
        let (kind, ch, key) = (status & 0xf0, status & 0x0f, key & 0x7f);
        if kind == 0x90 && vel > 0 {
            held.push((ch, key, at, vel));
        } else if kind == 0x80 || kind == 0x90 {
            if let Some(i) = held.iter().position(|h| h.0 == ch && h.1 == key) {
                let (_, _, start, vel) = held.remove(i);
                notes.push((start, at, key, vel, ch));
            }
        }
    }
    // Notes still held end with the source.
    for (ch, key, start, vel) in held {
        notes.push((start, at.max(start + 1), key, vel, ch));
    }
    MidiSource { ppq, notes, end: at }
}

/// The audio file of a ReaSamplOmatic5000: the first path to an audio file
/// in its (base64) state.
fn rs5k_sample(plugin: &Chunk) -> Option<String> {
    let mut data = Vec::new();
    for line in &plugin.body {
        if let Line::Values(v) = line {
            if let Some(bytes) = v.first().and_then(|s| crate::base64::decode(s)) {
                data.extend(bytes);
            }
        }
    }
    data.split(|&b| b < 0x20 || b == 0x7f).filter_map(|run| std::str::from_utf8(run).ok()).find_map(|s| {
        let s = s.trim();
        (s.contains(['/', '\\']) && ponzik_engine::wav::is_audio_file(std::path::Path::new(crate::flp::file_name_of(s))))
            .then(|| s.to_string())
    })
}

/// A plugin's name without its kind and vendor: "VSTi: ReaSynth (Cockos)" → "ReaSynth".
fn plugin_name(full: &str) -> (&str, bool) {
    let (kind, name) = full.split_once(": ").unwrap_or(("", full));
    let instrument = kind.ends_with('i') && !kind.is_empty();
    let name = name.rsplit_once(" (").map_or(name, |(n, _)| n);
    (name.trim(), instrument)
}

/// Converts a REAPER project into a new Ponzik project. `resolve` turns a
/// media path of the project into a readable path.
pub fn import_rpp(bytes: &[u8], name: &str, resolve: &dyn Fn(&str) -> Option<String>) -> Result<(Project, ImportReport), String> {
    if bytes.len() > MAX_FILE {
        return Err(tr("Not a REAPER project").into());
    }
    let text = String::from_utf8_lossy(bytes);
    if !text.trim_start().starts_with("<REAPER_PROJECT") {
        return Err(tr("Not a REAPER project").into());
    }
    let root = parse(&text)?;
    let mut b = Build::new("REAPER", name);
    if let Some(v) = root.head.get(2) {
        b.report.mapped.push(tr_fmt!("Saved by {}", format!("REAPER {v}")));
    }

    // Tempo, time signature and the tempo envelope.
    let bpm = root.num("TEMPO", 0).unwrap_or(120.0);
    let (num, den) = (root.num("TEMPO", 1).unwrap_or(4.0), root.num("TEMPO", 2).unwrap_or(4.0));
    b.signature(0, num as u32, den as u32);
    let mut points: Vec<(f64, f64, i64, Option<i64>)> = Vec::new();
    if let Some(env) = root.chunk("TEMPOENVEX") {
        for line in &env.body {
            if let Line::Values(v) = line {
                if v[0] != "PT" {
                    continue;
                }
                let num = |i: usize| v.get(i).and_then(|s| s.parse::<f64>().ok()).filter(|x| x.is_finite());
                if let (Some(t), Some(bpm)) = (num(1), num(2)) {
                    let shape = num(3).unwrap_or(1.0) as i64;
                    let sig = num(4).map(|s| s as i64).filter(|s| *s > 0);
                    points.push((t.max(0.0), bpm, shape, sig));
                }
            }
        }
        points.sort_by(|a, b| a.0.total_cmp(&b.0));
    }
    // Seconds → ticks along the envelope (a linear segment is a ramp).
    let first = points.first().filter(|p| p.0 <= 1e-9).map_or(bpm, |p| p.1);
    let mut changes = Vec::new();
    let (mut tick, mut prev) = (0.0f64, (0.0f64, first, 1i64));
    for (k, &(t, bpm, shape, sig)) in points.iter().enumerate() {
        if k == 0 && t <= 1e-9 {
            prev = (0.0, bpm, shape);
        } else {
            let ramp = prev.2 == 0;
            // A ramp moves linearly with the beats: its mean tempo over time
            // is the logarithmic mean of its ends.
            let avg = if ramp && prev.1 > 0.0 && bpm > 0.0 && (bpm - prev.1).abs() > 1e-9 {
                (bpm - prev.1) / (bpm / prev.1).ln()
            } else {
                prev.1
            };
            tick += (t - prev.0) * avg / 60.0 * PPQ as f64;
            changes.push((to_tick(tick), bpm, ramp));
            prev = (t, bpm, shape);
        }
        if let Some(s) = sig {
            b.signature(to_tick(tick), (s & 0xffff) as u32, ((s >> 16) & 0xffff) as u32);
        }
    }
    b.tempo(first, &changes);
    let map: TempoMap = b.p.tempo_map();
    let at = |secs: f64| to_tick(map.tick_at_secs(secs.max(0.0)));

    // Markers and regions (a region is written twice: its start, then its end).
    let mut regions = Vec::new();
    for line in &root.body {
        let Line::Values(v) = line else { continue };
        if v[0] != "MARKER" {
            continue;
        }
        let num = |i: usize| v.get(i).and_then(|s| s.parse::<f64>().ok()).filter(|x| x.is_finite());
        let (Some(idx), Some(pos)) = (num(1), num(2)) else { continue };
        let region = num(4).is_some_and(|f| f as i64 & 1 != 0);
        if region {
            if regions.contains(&(idx as i64)) {
                continue;
            }
            regions.push(idx as i64);
        }
        let color = num(5).map_or(0, |c| color_from(c as i64));
        b.marker(at(pos), v.get(3).map_or("", String::as_str), color);
    }
    if !regions.is_empty() {
        b.report.approximated.push(tr_fmt!("{} regions became markers at their start", regions.len()));
    }
    if root.num("LOOP", 0).unwrap_or(0.0) > 0.5 {
        if let (Some(s), Some(e)) = (root.num("SELECTION", 0), root.num("SELECTION", 1)) {
            let (s, e) = (at(s), at(e));
            if e > s {
                b.p.loop_range = Some((s, e));
                b.report.mapped.push(tr("Loop range").into());
            }
        }
    }

    // Tracks.
    let tracks: Vec<&Chunk> = root.chunks("TRACK").collect();
    let mut mixer = Vec::with_capacity(tracks.len());
    let mut folders: Vec<usize> = Vec::new();
    let (mut foldered, mut effects, mut envelopes, mut odd_rate, mut video) = (0, 0, 0, 0, 0);
    let mut sends: Vec<(usize, usize, f32)> = Vec::new();
    for (ti, t) in tracks.iter().enumerate() {
        let label = t.text("NAME").filter(|s| !s.trim().is_empty()).map_or_else(|| tr_fmt!("Track {}", ti + 1), str::to_string);
        let color = t.num("PEAKCOL", 0).map_or(0, |c| color_from(c as i64));
        let gain = t.num("VOLPAN", 0).unwrap_or(1.0);
        let pan = t.num("VOLPAN", 1).unwrap_or(0.0);
        let muted = t.num("MUTESOLO", 0).unwrap_or(0.0) > 0.5;
        let slot = b.track(&label, color, gain as f32, pan as f32, muted);
        mixer.push(slot);
        // Folders: the children go into their folder track.
        if let Some(&parent) = folders.last() {
            if route(&mut b.p, slot, parent) {
                foldered += 1;
            }
        }
        match t.num("ISBUS", 0).unwrap_or(0.0) as i64 {
            1 => folders.push(slot),
            2 => {
                let close = (-t.num("ISBUS", 1).unwrap_or(-1.0)).max(0.0) as usize;
                folders.truncate(folders.len().saturating_sub(close));
            }
            _ => {}
        }
        for line in &t.body {
            if let Line::Values(v) = line {
                if v[0] == "AUXRECV" {
                    let src = v.get(1).and_then(|s| s.parse::<usize>().ok());
                    let vol = v.get(3).and_then(|s| s.parse::<f32>().ok()).unwrap_or(1.0);
                    if let Some(src) = src {
                        sends.push((src, ti, finite(vol, 1.0)));
                    }
                }
            }
        }
        envelopes += t.body.iter().filter(|l| matches!(l, Line::Chunk(c) if c.name().ends_with("ENV2") || c.name() == "PARMENV")).count();

        // The instrument: the first instrument plugin, or the track's name.
        let mut instrument = None;
        if let Some(fx) = t.chunk("FXCHAIN") {
            for plug in fx.body.iter().filter_map(|l| match l {
                Line::Chunk(c) => Some(c),
                _ => None,
            }) {
                let full = plug.head.get(1).map_or("", String::as_str);
                let (pname, is_instrument) = plugin_name(full);
                if is_instrument && instrument.is_none() {
                    let kind = instrument_for_name(pname);
                    let mut def = InstrumentDef::new(kind);
                    if kind == "sampler" {
                        def.sample = rs5k_sample(plug).map(|raw| b.locate(&raw, resolve, None));
                    }
                    b.report.approximated.push(tr_fmt!("'{}': {} replaced by {} (settings not imported)", label, pname, crate::foreign::instrument_name(kind)));
                    instrument = Some(def);
                } else {
                    effects += 1;
                }
            }
        }
        let mut channel = None;
        let mut ensure_channel = |b: &mut Build, drums: bool, instrument: &mut Option<InstrumentDef>| -> usize {
            *channel.get_or_insert_with(|| {
                let def = instrument.take().unwrap_or_else(|| {
                    InstrumentDef::new(if drums { "drum_machine" } else { instrument_for_name(&label) })
                });
                b.channel(&label, def, slot, color)
            })
        };
        for item in t.chunks("ITEM") {
            let pos = item.num("POSITION", 0).unwrap_or(0.0).max(0.0);
            let len = item.num("LENGTH", 0).unwrap_or(0.0).max(0.0);
            let (start, end) = (at(pos), at(pos + len));
            let clip_len = end.saturating_sub(start).max(1);
            let looped = item.num("LOOP", 0).unwrap_or(0.0) > 0.5;
            let item_muted = item.num("MUTE", 0).unwrap_or(0.0) > 0.5;
            let item_vol = item.num("VOLPAN", 0).unwrap_or(1.0);
            let item_color = item.num("COLOR", 0).map_or(0, |c| color_from(c as i64));
            let all = takes(item);
            let take = all.iter().find(|t| t.selected).or(all.first()).expect("one take");
            let Some(mut src) = take.source else { continue };
            if (take.rate - 1.0).abs() > 1e-6 {
                odd_rate += 1;
            }
            // A section of a file: its start, maybe reversed.
            let (mut extra, mut reverse) = (0.0, false);
            while src.head.get(1).is_some_and(|k| k == "SECTION") {
                extra += src.num("STARTPOS", 0).unwrap_or(0.0);
                reverse ^= src.num("MODE", 0).is_some_and(|m| m as i64 & 2 != 0);
                match src.chunk("SOURCE") {
                    Some(inner) => src = inner,
                    None => break,
                }
            }
            let kind = src.head.get(1).map_or("", String::as_str);
            let item_name = take.name.or(item.text("NAME")).filter(|n| !n.trim().is_empty()).unwrap_or(label.as_str()).to_string();
            match kind {
                "MIDI" | "MIDIPOOL" => {
                    let midi = read_midi(src);
                    let scale = PPQ as f64 / midi.ppq;
                    let conv = |t: u64| to_tick(t as f64 * scale);
                    let drums = !midi.notes.is_empty() && midi.notes.iter().all(|n| n.4 == 9);
                    let ch = ensure_channel(&mut b, drums, &mut instrument);
                    let soffs = to_tick(map.tick_at_secs(pos + take.soffs.abs()) - start as f64) as i64 * take.soffs.signum() as i64;
                    let src_len = conv(midi.end).max(1);
                    let mut notes: Vec<Note> = midi
                        .notes
                        .iter()
                        .map(|&(s, e, key, vel, _)| {
                            let st = conv(s);
                            Note { start: st, len: conv(e).saturating_sub(st).max(1), key, vel: vel as f32 / 127.0, ..Note::DEFAULT }
                        })
                        .collect();
                    let (pat_len, offset) = if looped && midi.end > 0 {
                        (src_len, soffs.rem_euclid(src_len as i64) as u32)
                    } else if soffs < 0 {
                        // The item starts before its source: silence first.
                        let shift = (-soffs) as u32;
                        for n in &mut notes {
                            n.start += shift;
                        }
                        ((src_len + shift).max(clip_len), 0)
                    } else {
                        let off = soffs as u32;
                        (src_len.max(off + clip_len), off)
                    };
                    let pattern = b.pattern(ch, notes, pat_len, &item_name, item_color);
                    b.clip(pattern, ti, start, clip_len, offset, item_muted);
                }
                "WAVE" | "MP3" | "FLAC" | "VORBIS" | "OGG" | "OPUS" | "WAVPACK" | "AIFF" => {
                    let Some(raw) = src.text("FILE").filter(|f| !f.is_empty()) else { continue };
                    let path = b.locate(raw, resolve, None);
                    let fade = |key: &str| item.num(key, 1).unwrap_or(0.0).max(item.num(key, 2).unwrap_or(0.0));
                    b.audio(AudioClipDef {
                        path,
                        lane: ti,
                        track: slot,
                        start,
                        len: clip_len,
                        offset: take.soffs + extra,
                        gain: (item_vol * take.vol) as f32,
                        fade_in: fade("FADEIN") as f32,
                        fade_out: fade("FADEOUT") as f32,
                        muted: item_muted,
                        reverse,
                        looped,
                        color: item_color,
                        ..Default::default()
                    });
                }
                "VIDEO" => video += 1,
                _ => {}
            }
        }
        if let Some(def) = instrument.take() {
            // An instrument without MIDI items keeps its channel.
            b.channel(&label, def, slot, color);
        }
    }
    let mut send_count = 0;
    for (src, dst, vol) in sends {
        let (Some(&from), Some(&to)) = (mixer.get(src), mixer.get(dst)) else { continue };
        if from == MASTER || from == to {
            continue;
        }
        if let Some(s) = b.p.tracks[from].sends.iter_mut().find(|s| s.0 == MASTER && s.1 == 0.0) {
            *s = (to, vol.clamp(0.0, 4.0));
            send_count += 1;
        }
    }
    if foldered > 0 {
        b.report.mapped.push(tr_fmt!("Folders: {} tracks routed into their folder track", foldered));
    }
    if send_count > 0 {
        b.report.mapped.push(tr_fmt!("{} sends", send_count));
    }
    if odd_rate > 0 {
        b.report.approximated.push(tr_fmt!("{} items with another play rate play at normal speed", odd_rate));
    }
    if effects > 0 {
        b.report.skipped.push(tr_fmt!("{} effect plugins (not converted)", effects));
    }
    if envelopes > 0 {
        b.report.skipped.push(tr_fmt!("{} automation envelopes", envelopes));
    }
    if video > 0 {
        b.report.skipped.push(tr_fmt!("{} video items", video));
    }
    Ok(b.finish())
}

// --- Export ---

/// A string value, quoted the way REAPER reads it.
fn quote(s: &str) -> String {
    let s: String = s.chars().filter(|c| !c.is_control()).collect();
    if !s.contains('"') {
        format!("\"{s}\"")
    } else if !s.contains('\'') {
        format!("'{s}'")
    } else {
        format!("`{}`", s.replace('`', "'"))
    }
}

/// Seconds, written short.
fn secs(v: f64) -> String {
    let s = format!("{:.10}", if v.is_finite() { v } else { 0.0 });
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// MIDI ticks per quarter note in exported items (10 per Ponzik tick).
const EXPORT_PPQ: u32 = PPQ * 10;

fn midi_source(out: &mut String, notes: &[Note], len: u32) {
    out.push_str("      <SOURCE MIDI\n");
    let _ = writeln!(out, "        HASDATA 1 {EXPORT_PPQ} QN");
    // (tick, off before on, status, key, velocity)
    let mut events: Vec<(u32, u8, u8, u8, u8)> = Vec::new();
    for n in notes.iter().filter(|n| !n.slide) {
        let vel = (n.vel * 127.0).round().clamp(1.0, 127.0) as u8;
        events.push((n.start * 10, 1, 0x90, n.key & 0x7f, vel));
        events.push(((n.start + n.len.max(1)).min(len) * 10, 0, 0x80, n.key & 0x7f, 0));
    }
    events.sort_unstable();
    let mut at = 0;
    for (t, _, status, key, vel) in events {
        let _ = writeln!(out, "        E {} {status:02x} {key:02x} {vel:02x}", t - at);
        at = t;
    }
    // All notes off at the end: the source's length.
    let _ = writeln!(out, "        E {} b0 7b 00", (len * 10).saturating_sub(at));
    out.push_str("      >\n");
}

fn source_kind(path: &str) -> &'static str {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".mp3") {
        "MP3"
    } else if lower.ends_with(".flac") {
        "FLAC"
    } else if lower.ends_with(".ogg") {
        "VORBIS"
    } else if lower.ends_with(".opus") {
        "OPUS"
    } else {
        "WAVE"
    }
}

/// A REAPER copy of the project. `media` turns a sample path of the project
/// into a path on disk.
pub fn export_rpp(project: &Project, media: &dyn Fn(&str) -> Option<String>) -> String {
    let p = project;
    let map = p.tempo_map();
    let s = |tick: u32| secs(map.secs_at(tick as f64));
    let mut out = String::new();
    let first = p.signatures.iter().find(|s| s.tick == 0).copied().unwrap_or(ponzik_engine::TimeSig::FOUR_FOUR);
    out.push_str("<REAPER_PROJECT 0.1 \"7.0/Ponzik\" 0\n");
    out.push_str("  RIPPLE 0\n");
    let _ = writeln!(out, "  TEMPO {} {} {}", secs(p.bpm as f64), first.num, first.den);
    match p.loop_range {
        Some((a, b)) => {
            let _ = writeln!(out, "  LOOP 1\n  SELECTION {} {}", s(a), s(b));
        }
        None => out.push_str("  LOOP 0\n"),
    }
    // Tempo changes and time signature changes as tempo envelope points.
    if !p.tempo_changes.is_empty() || p.signatures.iter().any(|s| s.tick > 0) {
        let mut ticks: Vec<u32> = vec![0];
        ticks.extend(p.tempo_changes.iter().map(|t| t.tick));
        ticks.extend(p.signatures.iter().map(|s| s.tick));
        ticks.sort_unstable();
        ticks.dedup();
        out.push_str("  <TEMPOENVEX\n    ACT 1 -1\n    VIS 1 0 1\n    LANEHEIGHT 0 0\n    ARM 0\n    DEFSHAPE 1 -1 -1\n");
        for (k, &tick) in ticks.iter().enumerate() {
            let bpm = map.bpm_at(tick as f64);
            // The segment after a point is linear when the next change ramps.
            let next = ticks.get(k + 1).and_then(|&n| p.tempo_changes.iter().find(|t| t.tick == n));
            let shape = if next.is_some_and(|t| t.ramp) { 0 } else { 1 };
            let _ = write!(out, "    PT {} {} {shape}", s(tick), secs(bpm));
            if let Some(sig) = p.signatures.iter().find(|s| s.tick == tick) {
                let _ = write!(out, " {}", sig.num as u32 + ((sig.den as u32) << 16));
            }
            out.push('\n');
        }
        out.push_str("  >\n");
    }
    for (i, m) in p.markers.iter().enumerate() {
        let color = if m.color == 0 { 0 } else { color_to(m.color) };
        let _ = writeln!(out, "  MARKER {} {} {} 0 {color}", i + 1, s(m.tick), quote(&m.name));
    }
    // The playlist clips, or the current pattern when there are none.
    let mut clips: Vec<ClipDef> = p.clips.iter().filter(|c| !c.muted).copied().collect();
    if clips.is_empty() {
        let pattern = p.current_pattern.min(p.patterns.len().saturating_sub(1));
        if let Some(pat) = p.patterns.get(pattern) {
            clips.push(ClipDef { pattern, lane: 0, start: 0, len: pat.len.max(1), offset: 0, muted: false });
        }
    }
    clips.sort_by_key(|c| c.start);
    let track_head = |out: &mut String, name: &str, color: u32, gain: f32, pan: f32, muted: bool| {
        out.push_str("  <TRACK\n");
        let _ = writeln!(out, "    NAME {}", quote(name));
        let _ = writeln!(out, "    PEAKCOL {}", if color == 0 { 16576 } else { color_to(color) });
        let _ = writeln!(out, "    VOLPAN {} {} -1 -1 1", secs(gain as f64), secs(pan as f64));
        let _ = writeln!(out, "    MUTESOLO {} 0 0", muted as u8);
        out.push_str("    ISBUS 0 0\n    NCHAN 2\n");
    };
    for (ch, c) in p.channels.iter().enumerate() {
        let t = p.tracks.get(c.track);
        let gain = c.gain * t.map_or(1.0, |t| t.gain);
        let pan = if c.pan != 0.0 { c.pan } else { t.map_or(0.0, |t| t.pan) };
        let color = if c.color != 0 { c.color } else { t.map_or(0, |t| t.color) };
        track_head(&mut out, &c.name, color, gain, pan, c.muted || t.is_some_and(|t| t.muted));
        for clip in &clips {
            let notes = crate::als::clip_notes(p, clip, ch);
            if notes.is_empty() {
                continue;
            }
            let pat = &p.patterns[clip.pattern];
            out.push_str("    <ITEM\n");
            let _ = writeln!(out, "      POSITION {}\n      LENGTH {}", s(clip.start), secs(map.secs_at((clip.start + clip.len) as f64) - map.secs_at(clip.start as f64)));
            out.push_str("      LOOP 0\n      MUTE 0 0\n");
            let _ = writeln!(out, "      NAME {}", quote(&pat.name));
            if pat.color != 0 {
                let _ = writeln!(out, "      COLOR {} B", color_to(pat.color));
            }
            out.push_str("      VOLPAN 1 0 1 -1\n      SOFFS 0\n      PLAYRATE 1 1 0 -1 0 0.0025\n");
            midi_source(&mut out, &notes, clip.len);
            out.push_str("    >\n");
        }
        out.push_str("  >\n");
    }
    // Audio clips: one track per mixer track that plays some.
    let mut audio_tracks: Vec<usize> = p.audio_clips.iter().map(|c| c.track).collect();
    audio_tracks.sort_unstable();
    audio_tracks.dedup();
    for at in audio_tracks {
        let t = p.tracks.get(at);
        let name = t.map_or_else(|| tr("Audio").to_string(), |t| t.name.clone());
        track_head(&mut out, &name, t.map_or(0, |t| t.color), t.map_or(1.0, |t| t.gain), t.map_or(0.0, |t| t.pan), t.is_some_and(|t| t.muted));
        for c in p.audio_clips.iter().filter(|c| c.track == at) {
            let file = media(&c.path).unwrap_or_else(|| c.path.clone());
            out.push_str("    <ITEM\n");
            let _ = writeln!(out, "      POSITION {}\n      LENGTH {}", s(c.start), secs(map.secs_at((c.start + c.len) as f64) - map.secs_at(c.start as f64)));
            let _ = writeln!(out, "      LOOP {}\n      MUTE {} 0", c.looped as u8, c.muted as u8);
            let _ = writeln!(out, "      FADEIN 1 {} 0 1 0 0 0\n      FADEOUT 1 {} 0 1 0 0 0", secs(c.fade_in as f64), secs(c.fade_out as f64));
            if c.color != 0 {
                let _ = writeln!(out, "      COLOR {} B", color_to(c.color));
            }
            let _ = writeln!(out, "      NAME {}", quote(crate::flp::file_name_of(&file)));
            let _ = writeln!(out, "      VOLPAN 1 0 {} -1\n      SOFFS {}", secs(c.gain as f64), secs(c.offset));
            out.push_str("      PLAYRATE 1 1 0 -1 0 0.0025\n");
            let _ = writeln!(out, "      <SOURCE {}\n        FILE {}\n      >", source_kind(&file), quote(&file));
            out.push_str("    >\n");
        }
        out.push_str("  >\n");
    }
    out.push_str(">\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ponzik_engine::{PlayMode, TICKS_PER_BAR, TimeSig};

    /// A REAPER 7 project written by hand like REAPER writes them: a tempo
    /// envelope, markers and a region, a folder with a synth and a sampler,
    /// MIDI items (looped, offset, pooled), an audio track with fades and a
    /// reversed section, a send.
    const PROJECT: &str = r#"<REAPER_PROJECT 0.1 "7.11/linux-x86_64" 1700000000
  RIPPLE 0
  TEMPO 100 3 4
  PLAYRATE 1 0 0.25 4
  LOOP 1
  SELECTION 2.4 4.8
  <TEMPOENVEX
    EGUID {A}
    ACT 1 -1
    PT 0 100 1 262147 0 1
    PT 4.8 150 1
  >
  MARKER 1 0 "Intro" 0 0 1 R {M1} 0
  MARKER 2 3.6 Verse 0 33489151 1 R {M2} 0
  MARKER 3 4.8 "Chorus part" 1 0 1 R {M3} 0
  MARKER 3 6 "" 1
  <TRACK {T1}
    NAME "Band"
    PEAKCOL 16576
    VOLPAN 0.5 0 -1 -1 1
    MUTESOLO 0 0 0
    ISBUS 1 1
  >
  <TRACK {T2}
    NAME "Lead Synth"
    PEAKCOL 16777471
    VOLPAN 0.8 -0.5 -1 -1 1
    MUTESOLO 1 0 0
    ISBUS 0 0
    <FXCHAIN
      SHOW 0
      <VST "VSTi: Serum (Xfer Records)" Serum.vst3 0 "" 1234<5678> ""
        ZXZzbg==
      >
      <VST "VST: ReaEQ (Cockos)" reaeq.vst.so 0 "" 1919247729<56535472656571726561657100000000> ""
        cWVyZe5e
      >
    >
    <VOLENV2
      ACT 1 -1
      PT 0 1 0
    >
    <ITEM
      POSITION 1.2
      SNAPOFFS 0
      LENGTH 3.6
      LOOP 1
      FADEIN 1 0.01 0 1 0 0 0
      MUTE 0 0
      NAME "Riff"
      VOLPAN 1 0 1 -1
      SOFFS 0.6
      PLAYRATE 1 1 0 -1 0 0.0025
      <SOURCE MIDI
        HASDATA 1 960 QN
        CCINTERP 32
        E 0 90 3c 60
        e 480 80 3c 00
        Em 0 90 40 60
        E 480 80 40 00
        E 0 90 43 7f
        E 480 80 43 00
        <X 0 0
          /wAAAA==
        >
        E 1440 b0 7b 00
        IGNTEMPO 0 100 3 4
      >
    >
    <ITEM
      POSITION 0
      LENGTH 1.2
      LOOP 0
      NAME "Pickup"
      <SOURCE MIDI
        HASDATA 1 960 QN
        E 0 90 30 40
        E 960 80 30 00
        E 960 b0 7b 00
      >
    >
  >
  <TRACK {T3}
    NAME "Vox"
    PEAKCOL 33489151
    VOLPAN 1 0.25 -1 -1 1
    ISBUS 2 -1
    AUXRECV 1 0 0.5 0 0 0 0 0 0 -1:U 0 -1 ''
    <ITEM
      POSITION 2.4
      LENGTH 2.4
      LOOP 0
      FADEIN 1 0.25 0 1 0 0 0
      FADEOUT 1 0.01 0.5 1 0 0 0
      MUTE 1 0
      NAME "take 1"
      VOLPAN 0.5 0 0.5 -1
      SOFFS 1
      TAKE SEL
      NAME "take 2"
      TAKEVOLPAN 0 0.8 -1
      SOFFS 0.5
      <SOURCE SECTION
        LENGTH 3
        STARTPOS 0.25
        MODE 2
        <SOURCE WAVE
          FILE "Media/vox take 2.wav"
        >
      >
    >
    <ITEM
      POSITION 6
      LENGTH 1
      <SOURCE VIDEO
        FILE "clip.mp4"
      >
    >
  >
>
"#;

    #[test]
    fn tokens_and_chunks() {
        assert_eq!(tokens(r#"NAME "a b" 'c "d"' `e` f"#), ["NAME", "a b", "c \"d\"", "e", "f"]);
        assert_eq!(tokens("  "), Vec::<String>::new());
        assert_eq!(tokens(r#"NAME "unclosed"#), ["NAME", "unclosed"]);
        let root = parse(PROJECT).unwrap();
        assert_eq!(root.name(), "REAPER_PROJECT");
        assert_eq!(root.chunks("TRACK").count(), 3);
        assert_eq!(root.num("TEMPO", 0), Some(100.0));
        assert_eq!(color_from(16576), 0);
        assert_eq!(color_from(16777471), 0xff0000);
        assert_eq!(color_from(color_to(0x12ab34) as i64), 0x12ab34);
    }

    #[test]
    fn a_reaper_project_imports() {
        let (p, report) = import_rpp(PROJECT.as_bytes(), "Song", &|raw| Some(format!("/found/{raw}"))).unwrap();
        assert_eq!(report.source, "REAPER");
        // Tempo 100, then 150 at 4.8 s = 8 beats; 3/4.
        assert_eq!(p.bpm, 100.0);
        assert_eq!(p.tempo_changes.len(), 1);
        assert_eq!((p.tempo_changes[0].tick, p.tempo_changes[0].bpm, p.tempo_changes[0].ramp), (8 * PPQ, 150.0, false));
        assert_eq!(p.signatures, vec![TimeSig { tick: 0, num: 3, den: 4 }]);
        // Markers: Intro, Verse (magenta), the region's start only.
        let marks: Vec<(u32, &str)> = p.markers.iter().map(|m| (m.tick, m.name.as_str())).collect();
        assert_eq!(marks, [(0, "Intro"), (6 * PPQ, "Verse"), (8 * PPQ, "Chorus part")]);
        assert_eq!(p.markers[1].color, 0xff00ff);
        assert_eq!(p.loop_range, Some((4 * PPQ, 8 * PPQ)));
        // Tracks: the folder, its two children routed into it.
        assert_eq!(p.tracks[1].name, "Band");
        assert_eq!(p.tracks[1].gain, 0.5);
        assert_eq!((p.tracks[2].name.as_str(), p.tracks[2].output, p.tracks[2].muted), ("Lead Synth", 1, true));
        assert_eq!((p.tracks[2].gain, p.tracks[2].pan, p.tracks[2].color), (0.8, -0.5, 0xff0000));
        assert_eq!((p.tracks[3].output, p.tracks[3].pan), (1, 0.25));
        // The send from Lead Synth to Vox.
        assert_eq!(p.tracks[2].sends[0], (3, 0.5));
        // One channel: Serum → wavetable, on track 2.
        assert_eq!(p.channels.len(), 1);
        assert_eq!((p.channels[0].instrument.kind.as_str(), p.channels[0].track), ("wavetable", 2));
        // The looped riff: 3 beats (the muted note left out), entered 1 beat in.
        assert_eq!(p.clips.len(), 2);
        let riff = p.clips.iter().find(|c| c.start == 2 * PPQ).unwrap();
        assert_eq!((riff.len, riff.offset, riff.lane), (6 * PPQ, PPQ, 1));
        let pat = &p.patterns[riff.pattern];
        assert_eq!((pat.name.as_str(), pat.len), ("Riff", 3 * PPQ));
        assert_eq!(pat.notes[0].iter().map(|n| (n.start, n.len, n.key)).collect::<Vec<_>>(), [(0, PPQ / 2, 60), (PPQ, PPQ / 2, 67)]);
        // The pickup: not looped, 2 beats long in a 2-beat item.
        let pickup = p.clips.iter().find(|c| c.start == 0).unwrap();
        assert_eq!((pickup.len, p.patterns[pickup.pattern].len), (2 * PPQ, 2 * PPQ));
        // The audio item: the selected take, its section reversed and offset.
        assert_eq!(p.audio_clips.len(), 1);
        let a = &p.audio_clips[0];
        assert_eq!(a.path, "/found/Media/vox take 2.wav");
        assert_eq!((a.start, a.len, a.track, a.lane), (4 * PPQ, 4 * PPQ, 3, 2));
        assert!((a.offset - 0.75).abs() < 1e-9);
        assert!((a.gain - 0.4).abs() < 1e-6, "item volume times take volume");
        assert!((a.fade_in - 0.25).abs() < 1e-6 && (a.fade_out - 0.5).abs() < 1e-6);
        assert!(a.reverse && a.muted && !a.looped);
        assert_eq!(p.mode, PlayMode::Song);
        assert!(report.skipped.iter().any(|s| s.contains("1 effect plugins")));
        assert!(report.skipped.iter().any(|s| s.contains("1 automation envelopes")));
        assert!(report.skipped.iter().any(|s| s.contains("1 video items")));
        assert!(report.approximated.iter().any(|s| s.contains("Serum")));
    }

    #[test]
    fn a_project_comes_back_through_the_importer() {
        let mut p = crate::demo_project();
        p.tempo_changes.push(ponzik_engine::TempoPoint { tick: TICKS_PER_BAR * 4, bpm: 140.0, ramp: true });
        p.signatures.push(TimeSig { tick: TICKS_PER_BAR * 8, num: 7, den: 8 });
        p.markers.push(crate::Marker { tick: TICKS_PER_BAR * 2, name: "Drop \"big\"".into(), color: 0x00ff00 });
        p.loop_range = Some((0, TICKS_PER_BAR * 2));
        p.audio_clips.push(AudioClipDef {
            path: "/s/loop.wav".into(),
            start: TICKS_PER_BAR,
            len: TICKS_PER_BAR * 2,
            offset: 0.5,
            gain: 0.7,
            fade_in: 0.1,
            track: 3,
            ..Default::default()
        });
        let text = export_rpp(&p, &|s| Some(s.to_string()));
        let (q, _) = import_rpp(text.as_bytes(), "Back", &|s| Some(s.to_string())).unwrap();
        assert!((q.bpm - p.bpm).abs() < 1e-3);
        assert_eq!(q.tempo_changes.len(), 1);
        assert_eq!(q.tempo_changes[0].tick, TICKS_PER_BAR * 4);
        assert!(q.tempo_changes[0].ramp);
        assert_eq!(q.signatures, p.signatures);
        assert_eq!(q.markers.len(), 1);
        assert_eq!((q.markers[0].tick, q.markers[0].name.as_str(), q.markers[0].color), (TICKS_PER_BAR * 2, "Drop \"big\"", 0x00ff00));
        assert_eq!(q.loop_range, p.loop_range);
        assert_eq!(q.channels.iter().map(|c| c.name.clone()).collect::<Vec<_>>(), p.channels.iter().map(|c| c.name.clone()).collect::<Vec<_>>());
        let song = |p: &Project| {
            let mut v = Vec::new();
            for c in p.clips.iter().filter(|c| !c.muted) {
                for ch in 0..p.channels.len() {
                    for n in crate::als::clip_notes(p, c, ch) {
                        v.push((ch, c.start + n.start, n.key, n.len));
                    }
                }
            }
            v.sort_unstable();
            v
        };
        assert_eq!(song(&q), song(&p));
        assert_eq!(q.audio_clips.len(), 1);
        let a = &q.audio_clips[0];
        assert_eq!((a.path.as_str(), a.start, a.len), ("/s/loop.wav", TICKS_PER_BAR, TICKS_PER_BAR * 2));
        assert!((a.offset - 0.5).abs() < 1e-9 && (a.gain - 0.7).abs() < 1e-6 && (a.fade_in - 0.1).abs() < 1e-6);
    }

    #[test]
    fn junk_is_an_error_or_an_empty_project_never_a_panic() {
        assert!(import_rpp(b"", "x", &|_| None).is_err());
        assert!(import_rpp(b"<REAPER_PROJEKT", "x", &|_| None).is_err());
        assert!(import_rpp(&[0xff, 0xfe, 0x00, 0x3c], "x", &|_| None).is_err());
        let (p, _) = import_rpp(b"<REAPER_PROJECT\n", "x", &|_| None).unwrap();
        assert!(p.channels.is_empty() && !p.patterns.is_empty());
        // Every truncation of a real project reads without panicking.
        for cut in (0..PROJECT.len()).step_by(7) {
            if PROJECT.is_char_boundary(cut) {
                let _ = import_rpp(&PROJECT.as_bytes()[..cut], "x", &|_| None);
            }
        }
        // Garbage values.
        let junk = "<REAPER_PROJECT\nTEMPO nan -3 0\n<TEMPOENVEX\nPT -1 0 9 99999999\nPT inf 1e30\n>\nMARKER x\nMARKER 1 1e300 a 1\n<TRACK\nVOLPAN inf nan\nISBUS 2 -99\n<ITEM\nPOSITION -5\nLENGTH 1e300\n<SOURCE MIDI\nHASDATA 1 0 QN\nE 99999999999999 90 zz 10\nE -5 90 3c 10\n>\n>\n";
        let (p, _) = import_rpp(junk.as_bytes(), "x", &|_| None).unwrap();
        assert!(p.bpm.is_finite() && p.tracks[1].gain.is_finite());
        // Deep nesting is skipped, not recursed into.
        let deep = format!("<REAPER_PROJECT\n{}", "<A\n".repeat(100_000));
        assert!(import_rpp(deep.as_bytes(), "x", &|_| None).is_ok());
    }
}
