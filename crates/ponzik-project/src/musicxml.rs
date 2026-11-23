//! MusicXML scores (`.musicxml`, `.xml`, and `.mxl`: the same in a zip
//! archive whose `META-INF/container.xml` names the score).
//!
//! A partwise score lists its parts, then each part's measures; a note lasts
//! a number of `divisions` of a quarter note, `<chord/>` stacks it on the
//! previous one, `<backup>` and `<forward>` move the cursor (to write other
//! voices), ties join notes across beats and bars.
//!
//! Import makes one track and one channel per part (named, the closest
//! Ponzik instrument from its MIDI program or its name, its volume and pan),
//! the part's notes as one pattern placed at the start of the playlist,
//! sounding pitches (transposing instruments are transposed back), the
//! tempo marks and time signatures. Measures are as long as their longest
//! part. Repeats are not unrolled; lyrics and notation marks stay behind.

use ponzik_engine::{Note, PPQ};
use ponzik_i18n::{tr, tr_fmt};

use crate::als::{Element, parse_xml};
use crate::foreign::{Build, MAX_FILE, MAX_TICK, instrument_for_name, instrument_for_program, to_tick};
use crate::{ImportReport, InstrumentDef, Project};

fn num(e: &Element, key: &str) -> Option<f64> {
    e.attr(key)?.trim().parse().ok().filter(|v: &f64| v.is_finite())
}

fn text_num(e: &Element, child: &str) -> Option<f64> {
    e.child_text(child)?.parse().ok().filter(|v: &f64| v.is_finite())
}

/// The score's XML: the file itself, or the score inside a `.mxl` archive.
fn score_text(bytes: &[u8]) -> Result<String, String> {
    let raw = if crate::zip::is_zip(bytes) {
        let entries = crate::zip::read_zip(bytes).map_err(|e| tr_fmt!("Not a readable MusicXML archive: {}", e))?;
        let named = crate::zip::extract_named(bytes, &entries, "META-INF/container.xml", 1 << 20)
            .and_then(Result::ok)
            .and_then(|c| {
                let root = parse_xml(&String::from_utf8_lossy(&c)).ok()?;
                let mut files = Vec::new();
                root.find_all("rootfile", &mut files);
                files.first().and_then(|f| f.attr("full-path")).map(str::to_string)
            });
        let entry = named
            .and_then(|n| entries.iter().find(|e| e.name == n))
            .or_else(|| {
                entries.iter().find(|e| {
                    let n = e.name.to_ascii_lowercase();
                    !n.starts_with("meta-inf/") && (n.ends_with(".musicxml") || n.ends_with(".xml"))
                })
            })
            .ok_or(tr("No score in this MusicXML archive"))?;
        crate::zip::extract(bytes, entry, MAX_FILE).map_err(|e| tr_fmt!("Not a readable MusicXML archive: {}", e))?
    } else {
        bytes.to_vec()
    };
    // UTF-16 files start with their byte order mark.
    let utf16 = |le: bool| {
        let units: Vec<u16> =
            raw[2..].chunks_exact(2).map(|c| if le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) }).collect();
        String::from_utf16_lossy(&units)
    };
    Ok(match raw.get(..2) {
        Some([0xff, 0xfe]) => utf16(true),
        Some([0xfe, 0xff]) => utf16(false),
        _ => String::from_utf8_lossy(&raw).into_owned(),
    })
}

/// Semitone of a step letter.
fn step(s: &str) -> Option<i32> {
    Some(match s.trim().to_ascii_uppercase().as_str() {
        "C" => 0,
        "D" => 2,
        "E" => 4,
        "F" => 5,
        "G" => 7,
        "A" => 9,
        "B" => 11,
        _ => return None,
    })
}

/// A note of a part: its measure, its place in it and length (ticks), key, velocity.
struct PartNote {
    measure: usize,
    at: f64,
    len: f64,
    key: u8,
    vel: f32,
}

/// What a part says about itself in the part list.
#[derive(Default)]
struct PartInfo {
    name: String,
    program: Option<u8>,
    channel: Option<u8>,
    volume: Option<f64>,
    pan: Option<f64>,
    /// Score instrument id → MIDI key (percussion).
    unpitched: Vec<(String, u8)>,
}

fn part_info(list: Option<&Element>, id: &str) -> PartInfo {
    let Some(sp) = list.and_then(|l| l.children_named("score-part").find(|p| p.attr("id") == Some(id))) else {
        return PartInfo::default();
    };
    let mut info = PartInfo {
        name: sp.child_text("part-name").filter(|n| !n.is_empty()).or_else(|| sp.child_text("part-abbreviation")).unwrap_or("").to_string(),
        ..Default::default()
    };
    for mi in sp.children_named("midi-instrument") {
        let n = |k: &str| text_num(mi, k);
        info.program = info.program.or(n("midi-program").map(|p| (p.clamp(1.0, 128.0) - 1.0) as u8));
        info.channel = info.channel.or(n("midi-channel").map(|c| (c.clamp(1.0, 16.0) - 1.0) as u8));
        info.volume = info.volume.or(n("volume"));
        info.pan = info.pan.or(n("pan"));
        if let (Some(id), Some(key)) = (mi.attr("id"), n("midi-unpitched")) {
            info.unpitched.push((id.to_string(), (key.clamp(1.0, 128.0) - 1.0) as u8));
        }
    }
    info
}

/// Converts a MusicXML score into a new Ponzik project.
pub fn import_musicxml(bytes: &[u8], name: &str) -> Result<(Project, ImportReport), String> {
    let text = score_text(bytes)?;
    let root = parse_xml(&text)?;
    match root.name.as_str() {
        "score-partwise" => {}
        "score-timewise" => return Err(tr("Timewise MusicXML scores are not supported yet (only partwise ones)").into()),
        _ => return Err(tr("Not a MusicXML score").into()),
    }
    let title = root
        .at(&["work", "work-title"])
        .map(|t| t.text.trim())
        .or_else(|| root.child_text("movement-title"))
        .filter(|t| !t.is_empty())
        .unwrap_or(name);
    let mut b = Build::new("MusicXML", title);
    if let Some(sw) = root.at(&["identification", "encoding", "software"]).map(|s| s.text.trim()).filter(|s| !s.is_empty()) {
        b.report.mapped.push(tr_fmt!("Saved by {}", sw));
    }
    let list = root.child("part-list");
    let parts: Vec<&Element> = root.children_named("part").collect();

    // First pass: each part's notes by measure, the measures' lengths, tempo and meter marks.
    let mut notes_of: Vec<Vec<PartNote>> = Vec::new();
    let mut lengths: Vec<f64> = Vec::new();
    // (measure, place, BPM) and (measure, beats, beat type), from any part.
    let mut tempos: Vec<(usize, f64, f64)> = Vec::new();
    let mut meters: Vec<(usize, u32, u32)> = Vec::new();
    let (mut repeats, mut lyrics, mut grace) = (false, 0, 0);
    for part in &parts {
        let info = part_info(list, part.attr("id").unwrap_or(""));
        let mut out: Vec<PartNote> = Vec::new();
        let mut divisions = 1.0f64;
        let mut transpose = 0i32;
        let mut dynamics = 80.0f64;
        // Notes tied forward: index in `out` by key.
        let mut open_ties: Vec<(u8, usize)> = Vec::new();
        let mut last_start = 0.0;
        for (mi, measure) in part.children_named("measure").enumerate() {
            if mi > 100_000 {
                break;
            }
            let (mut cursor, mut furthest) = (0.0f64, 0.0f64);
            let ticks = |d: f64, div: f64| d.max(0.0) * PPQ as f64 / div;
            for el in &measure.children {
                match el.name.as_str() {
                    "attributes" => {
                        if let Some(d) = text_num(el, "divisions").filter(|d| *d > 0.0) {
                            divisions = d;
                        }
                        if let Some(t) = el.child("time") {
                            if let (Some(beats), Some(kind)) = (text_num(t, "beats"), text_num(t, "beat-type")) {
                                meters.push((mi, beats as u32, kind as u32));
                            }
                        }
                        if let Some(t) = el.child("transpose") {
                            transpose = text_num(t, "chromatic").unwrap_or(0.0) as i32 + 12 * text_num(t, "octave-change").unwrap_or(0.0) as i32;
                        }
                    }
                    "backup" => cursor = (cursor - ticks(text_num(el, "duration").unwrap_or(0.0), divisions)).max(0.0),
                    "forward" => {
                        cursor += ticks(text_num(el, "duration").unwrap_or(0.0), divisions);
                        furthest = furthest.max(cursor);
                    }
                    "direction" | "sound" => {
                        let mut sounds = Vec::new();
                        if el.name == "sound" {
                            sounds.push(el);
                        } else {
                            el.find_all("sound", &mut sounds);
                        }
                        for s in sounds {
                            if let Some(t) = num(s, "tempo").filter(|t| *t > 0.0) {
                                tempos.push((mi, cursor, t));
                            }
                            if let Some(d) = num(s, "dynamics") {
                                dynamics = d;
                            }
                        }
                    }
                    "barline" => repeats |= el.child("repeat").is_some() || el.child("ending").is_some(),
                    "note" => {
                        if el.child("grace").is_some() || el.child("cue").is_some() {
                            grace += 1;
                            continue;
                        }
                        lyrics += el.children_named("lyric").count();
                        let dur = ticks(text_num(el, "duration").unwrap_or(0.0), divisions);
                        let chord = el.child("chord").is_some();
                        let start = if chord { last_start } else { cursor };
                        if !chord {
                            cursor += dur;
                            furthest = furthest.max(cursor);
                            last_start = start;
                        }
                        let key = if let Some(p) = el.child("pitch") {
                            let Some(s) = p.child_text("step").and_then(step) else { continue };
                            let alter = text_num(p, "alter").unwrap_or(0.0).round() as i32;
                            let octave = text_num(p, "octave").unwrap_or(4.0) as i32;
                            (octave + 1) * 12 + s + alter + transpose
                        } else if let Some(u) = el.child("unpitched") {
                            let inst = el.child("instrument").and_then(|i| i.attr("id"));
                            match inst.and_then(|id| info.unpitched.iter().find(|u| u.0 == id)) {
                                Some(&(_, key)) => key as i32,
                                None => {
                                    let s = u.child_text("display-step").and_then(step).unwrap_or(0);
                                    (text_num(u, "display-octave").unwrap_or(4.0) as i32 + 1) * 12 + s
                                }
                            }
                        } else {
                            continue;
                        };
                        let key = key.clamp(0, 127) as u8;
                        let ties: Vec<&str> = el.children_named("tie").filter_map(|t| t.attr("type")).collect();
                        let vel = (num(el, "dynamics").unwrap_or(dynamics) * 0.9 / 127.0).clamp(0.05, 1.0) as f32;
                        let tied = ties.contains(&"stop").then(|| open_ties.iter().position(|t| t.0 == key)).flatten();
                        let index = match tied {
                            Some(i) => {
                                let (_, at) = open_ties.remove(i);
                                if let Some(n) = out.get_mut(at) {
                                    n.len += dur;
                                }
                                at
                            }
                            None => {
                                out.push(PartNote { measure: mi, at: start, len: dur, key, vel });
                                out.len() - 1
                            }
                        };
                        if ties.contains(&"start") {
                            open_ties.push((key, index));
                        }
                    }
                    _ => {}
                }
            }
            if lengths.len() <= mi {
                lengths.resize(mi + 1, 0.0);
            }
            lengths[mi] = lengths[mi].max(furthest);
        }
        notes_of.push(out);
    }

    // Time signatures and the measures' starts.
    meters.sort_by_key(|m| m.0);
    meters.dedup_by_key(|m| m.0);
    let mut starts = Vec::with_capacity(lengths.len() + 1);
    let mut at = 0.0f64;
    let mut sig = (4u32, 4u32);
    let mut mi_sig = meters.iter().peekable();
    for (mi, &len) in lengths.iter().enumerate() {
        while let Some(&&(m, n, d)) = mi_sig.peek() {
            if m > mi {
                break;
            }
            sig = (n, d);
            b.signature(to_tick(at), n, d);
            mi_sig.next();
        }
        starts.push(at);
        let bar = sig.0.max(1) as f64 * PPQ as f64 * 4.0 / sig.1.max(1) as f64;
        at += if len > 0.0 { len } else { bar };
        if at >= MAX_TICK as f64 {
            break;
        }
    }
    let total = to_tick(at).max(1);
    let place = |m: usize, local: f64| starts.get(m).map(|s| to_tick(s + local));

    tempos.sort_by(|a, b| (a.0, a.1).partial_cmp(&(b.0, b.1)).unwrap_or(std::cmp::Ordering::Equal));
    let marks: Vec<(u32, f64)> = tempos.iter().filter_map(|&(m, l, t)| Some((place(m, l)?, t))).collect();
    let first = marks.iter().find(|m| m.0 == 0).map_or(marks.first().map_or(120.0, |m| m.1), |m| m.1);
    let changes: Vec<(u32, f64, bool)> = marks.iter().filter(|m| m.0 > 0).map(|&(t, bpm)| (t, bpm, false)).collect();
    b.tempo(first, &changes);

    // Second pass: one track, one channel and one pattern per part.
    for (pi, (part, notes)) in parts.iter().zip(notes_of).enumerate() {
        let info = part_info(list, part.attr("id").unwrap_or(""));
        let label = if info.name.is_empty() { tr_fmt!("Part {}", pi + 1) } else { info.name.clone() };
        let gain = info.volume.map_or(1.0, |v| (v / 80.0).clamp(0.0, 1.25)) as f32;
        let pan = info.pan.map_or(0.0, |p| (p / 90.0).clamp(-1.0, 1.0)) as f32;
        let track = b.track(&label, 0, gain, pan, false);
        let kind = match (info.program, info.channel) {
            (_, Some(9)) => "drum_machine",
            (Some(p), c) => instrument_for_program(p, c.unwrap_or(0)),
            (None, _) if !info.unpitched.is_empty() => "drum_machine",
            (None, _) => instrument_for_name(&label),
        };
        let ch = b.channel(&label, InstrumentDef::new(kind), track, 0);
        let notes: Vec<Note> = notes
            .iter()
            .filter_map(|n| {
                let start = place(n.measure, n.at)?;
                let end = place(n.measure, n.at + n.len)?;
                Some(Note { start, len: end.saturating_sub(start).max(1), key: n.key, vel: n.vel, ..Note::DEFAULT })
            })
            .collect();
        let pattern = b.pattern(ch, notes, total, &label, 0);
        b.clip(pattern, pi, 0, total, 0, false);
    }
    if !parts.is_empty() {
        b.report.mapped.push(tr_fmt!("{} parts, each a track with its notes in one pattern", parts.len()));
    }
    if repeats {
        b.report.approximated.push(tr("Repeats and endings are played once (not unrolled)").into());
    }
    if grace > 0 {
        b.report.skipped.push(tr_fmt!("{} grace and cue notes", grace));
    }
    if lyrics > 0 {
        b.report.skipped.push(tr_fmt!("{} lyrics", lyrics));
    }
    b.report.skipped.push(tr("Articulations, dynamics marks and the layout of the score").into());
    Ok(b.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ponzik_engine::TimeSig;

    const SCORE: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="no"?>
<!DOCTYPE score-partwise PUBLIC "-//Recordare//DTD MusicXML 4.0 Partwise//EN" "http://www.musicxml.org/dtds/partwise.dtd">
<score-partwise version="4.0">
  <work><work-title>Little Tune</work-title></work>
  <identification><encoding><software>MuseScore 4.2</software></encoding></identification>
  <part-list>
    <score-part id="P1">
      <part-name>Clarinet in B&#x266d;</part-name>
      <score-instrument id="P1-I1"><instrument-name>Clarinet</instrument-name></score-instrument>
      <midi-instrument id="P1-I1"><midi-channel>1</midi-channel><midi-program>72</midi-program><volume>40</volume><pan>-45</pan></midi-instrument>
    </score-part>
    <score-part id="P2">
      <part-name>Drumset</part-name>
      <score-instrument id="P2-I36"><instrument-name>Bass Drum</instrument-name></score-instrument>
      <midi-instrument id="P2-I36"><midi-channel>10</midi-channel><midi-unpitched>37</midi-unpitched></midi-instrument>
    </score-part>
  </part-list>
  <part id="P1">
    <measure number="1">
      <attributes><divisions>2</divisions><key><fifths>0</fifths></key><time><beats>3</beats><beat-type>4</beat-type></time>
        <transpose><diatonic>-1</diatonic><chromatic>-2</chromatic></transpose></attributes>
      <direction placement="above"><direction-type><metronome><beat-unit>quarter</beat-unit><per-minute>90</per-minute></metronome></direction-type><sound tempo="90"/></direction>
      <note><pitch><step>D</step><octave>4</octave></pitch><duration>2</duration><voice>1</voice><type>quarter</type></note>
      <note><chord/><pitch><step>F</step><alter>1</alter><octave>4</octave></pitch><duration>2</duration><voice>1</voice></note>
      <note><rest/><duration>2</duration><voice>1</voice></note>
      <note><pitch><step>E</step><octave>4</octave></pitch><duration>2</duration><tie type="start"/><voice>1</voice></note>
    </measure>
    <measure number="2">
      <note><pitch><step>E</step><octave>4</octave></pitch><duration>4</duration><tie type="stop"/><voice>1</voice></note>
      <note><grace/><pitch><step>G</step><octave>4</octave></pitch><voice>1</voice></note>
      <note dynamics="141.11"><pitch><step>G</step><octave>4</octave></pitch><duration>2</duration><voice>1</voice><lyric><text>la</text></lyric></note>
      <backup><duration>6</duration></backup>
      <note><pitch><step>C</step><octave>3</octave></pitch><duration>6</duration><voice>2</voice></note>
      <barline location="right"><repeat direction="backward"/></barline>
    </measure>
    <measure number="3">
      <attributes><time><beats>2</beats><beat-type>4</beat-type></time></attributes>
      <sound tempo="120"/>
      <note><pitch><step>C</step><octave>5</octave></pitch><duration>4</duration></note>
    </measure>
  </part>
  <part id="P2">
    <measure number="1">
      <attributes><divisions>1</divisions></attributes>
      <note><unpitched><display-step>F</display-step><display-octave>4</display-octave></unpitched><duration>1</duration><instrument id="P2-I36"/></note>
      <forward><duration>1</duration></forward>
      <note><unpitched><display-step>C</display-step><display-octave>5</display-octave></unpitched><duration>1</duration></note>
    </measure>
  </part>
</score-partwise>
"#;

    #[test]
    fn a_score_imports() {
        let (p, report) = import_musicxml(SCORE.as_bytes(), "file").unwrap();
        assert_eq!(report.source, "MusicXML");
        assert_eq!(p.name, "Little Tune");
        assert_eq!(p.bpm, 90.0);
        // 120 BPM from measure 3 (two bars of 3/4 = 6 beats).
        assert_eq!(p.tempo_changes.len(), 1);
        assert_eq!((p.tempo_changes[0].tick, p.tempo_changes[0].bpm), (6 * PPQ, 120.0));
        assert_eq!(p.signatures, vec![TimeSig { tick: 0, num: 3, den: 4 }, TimeSig { tick: 6 * PPQ, num: 2, den: 4 }]);
        assert_eq!(p.channels.len(), 2);
        let clar = &p.channels[0];
        assert_eq!((clar.name.as_str(), clar.instrument.kind.as_str()), ("Clarinet in B\u{266d}", "winds"));
        assert!((p.tracks[1].gain - 0.5).abs() < 1e-6 && (p.tracks[1].pan + 0.5).abs() < 1e-6);
        assert_eq!(p.channels[1].instrument.kind, "drum_machine");
        // One pattern per part, as long as the score (6 + 2 beats), placed at 0.
        assert_eq!(p.clips.len(), 2);
        assert!(p.clips.iter().all(|c| c.start == 0 && c.len == 8 * PPQ));
        let pat = &p.patterns[p.clips[0].pattern];
        let notes: Vec<(u32, u32, u8)> = pat.notes[0].iter().map(|n| (n.start, n.len, n.key)).collect();
        // Sounding pitch: a whole tone below the written one; E tied over the bar.
        assert_eq!(
            notes,
            [(0, PPQ, 60), (0, PPQ, 64), (2 * PPQ, 3 * PPQ, 62), (3 * PPQ, 3 * PPQ, 46), (5 * PPQ, PPQ, 65), (6 * PPQ, 2 * PPQ, 70)]
        );
        let loud = pat.notes[0].iter().find(|n| n.key == 65).unwrap();
        assert!((loud.vel - 1.0).abs() < 1e-3);
        let drums = &p.patterns[p.clips[1].pattern].notes[1];
        assert_eq!(drums.iter().map(|n| (n.start, n.key)).collect::<Vec<_>>(), [(0, 36), (2 * PPQ, 72)]);
        assert!(report.approximated.iter().any(|s| s.contains("Repeats")));
        assert!(report.skipped.iter().any(|s| s.contains("1 grace")));
        assert!(report.skipped.iter().any(|s| s.contains("1 lyrics")));
    }

    #[test]
    fn mxl_archives_and_utf16_files() {
        let mut w = crate::zip::ZipWriter::new();
        w.add("mimetype", b"application/vnd.recordare.musicxml", false);
        w.add(
            "META-INF/container.xml",
            br#"<container><rootfiles><rootfile full-path="score/tune.musicxml" media-type="application/vnd.recordare.musicxml+xml"/></rootfiles></container>"#,
            true,
        );
        w.add("score/tune.musicxml", SCORE.as_bytes(), true);
        let (a, _) = import_musicxml(&w.finish(), "x").unwrap();
        let (b, _) = import_musicxml(SCORE.as_bytes(), "x").unwrap();
        assert_eq!(a.patterns, b.patterns);
        // No container: the first score file.
        let mut w = crate::zip::ZipWriter::new();
        w.add("tune.xml", SCORE.as_bytes(), true);
        assert_eq!(import_musicxml(&w.finish(), "x").unwrap().0.patterns, b.patterns);
        let mut utf16 = vec![0xff, 0xfe];
        utf16.extend(SCORE.encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(import_musicxml(&utf16, "x").unwrap().0.patterns, b.patterns);
    }

    #[test]
    fn junk_is_an_error_never_a_panic() {
        assert!(import_musicxml(b"", "x").is_err());
        assert!(import_musicxml(b"<score-timewise/>", "x").unwrap_err().contains("Timewise"));
        assert!(import_musicxml(b"<html/>", "x").is_err());
        assert!(import_musicxml(b"PK\x03\x04junk", "x").is_err());
        let mut w = crate::zip::ZipWriter::new();
        w.add("readme.txt", b"hi", false);
        assert!(import_musicxml(&w.finish(), "x").is_err());
        for cut in (0..SCORE.len()).step_by(9) {
            let _ = import_musicxml(&SCORE.as_bytes()[..cut], "x");
        }
        let junk = r#"<score-partwise><part id="P1"><measure><attributes><divisions>0</divisions><time><beats>999</beats><beat-type>3</beat-type></time></attributes>
            <note><pitch><step>H</step><octave>99</octave></pitch><duration>1e300</duration></note><note><chord/><pitch><step>C</step><octave>-9</octave></pitch><duration>nan</duration><tie type="stop"/></note>
            <backup><duration>1e300</duration></backup><sound tempo="-4"/></measure></part></score-partwise>"#;
        let (p, _) = import_musicxml(junk.as_bytes(), "x").unwrap();
        assert!(p.bpm.is_finite());
    }
}
