//! LMMS projects (`.mmp`, and `.mmpz`: the same XML compressed by Qt's
//! `qCompress`, a 4-byte size and a zlib stream).
//!
//! Positions are LMMS ticks, 48 per quarter note (192 per 4/4 bar). The song
//! editor holds instrument tracks (their patterns of notes), sample tracks
//! (their sample clips), Beat/Bassline tracks (clips that play a beat
//! pattern of the Beat/Bassline editor) and automation tracks. Every track
//! goes to an FX channel of the FX mixer.
//!
//! Import reads the tempo and time signature, the loop, each instrument
//! track as a channel (name, colour, volume, pan, mute, the closest Ponzik
//! instrument; AudioFileProcessor becomes a sampler with its sample), its
//! patterns as playlist clips, each Beat/Bassline as one pattern of all the
//! beat channels with its clips, sample clips as audio clips, and the FX
//! mixer (channels as mixer tracks with name, volume and mute, their
//! routing and sends). Automation and effects stay behind.

use ponzik_engine::mixer::MASTER;
use ponzik_engine::{Note, PPQ, TICKS_PER_STEP};
use ponzik_i18n::{tr, tr_fmt};

use crate::als::{Element, parse_xml};
use crate::foreign::{Build, MAX_FILE, MAX_TICK, finite, hex_color, instrument_for_name, instrument_for_program, route, to_tick};
use crate::{AudioClipDef, ImportReport, InstrumentDef, Project};

/// LMMS ticks per quarter note.
const LMMS_PPQ: f64 = 48.0;
/// LMMS ticks of a bar of 4/4 (the unit of Beat/Bassline positions).
const LMMS_BAR: f64 = 192.0;
/// LMMS's default base note (A4 in its numbering, 440 Hz).
const LMMS_BASE: i32 = 57;

fn num(e: &Element, key: &str) -> Option<f64> {
    e.attr(key)?.trim().parse().ok().filter(|v: &f64| v.is_finite())
}

/// LMMS ticks → Ponzik ticks.
fn tick(t: f64) -> u32 {
    to_tick(t * PPQ as f64 / LMMS_PPQ)
}

/// The first child among `names` (element names changed in LMMS 1.3).
fn child_of<'a>(e: &'a Element, names: &[&str]) -> Option<&'a Element> {
    e.children.iter().find(|c| names.contains(&c.name.as_str()))
}

fn children_of<'a>(e: &'a Element, names: &'a [&'a str]) -> impl Iterator<Item = &'a Element> + 'a {
    e.children.iter().filter(move |c| names.contains(&c.name.as_str()))
}

const PATTERN: &[&str] = &["pattern", "midiclip"];
const SAMPLE_CLIP: &[&str] = &["sampletco", "sampleclip"];
const BB_CLIP: &[&str] = &["bbtco", "patternclip"];

/// Finds a sample of an LMMS project: the path as written, next to the
/// project, then in LMMS's sample folders (its factory samples are relative
/// to them).
pub fn resolve_lmms_sample(raw: &str, project_dir: Option<&std::path::Path>) -> Option<String> {
    if let Some(found) = crate::flp::resolve_sample(raw, project_dir) {
        return Some(found);
    }
    let rel = raw.replace('\\', "/");
    let rel = rel.trim_start_matches("factorysample:").trim_start_matches("userprojects:").trim_start_matches("usersample:");
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(std::path::PathBuf::from);
    let mut roots: Vec<std::path::PathBuf> = Vec::new();
    if let Some(h) = &home {
        roots.push(h.join("lmms").join("samples"));
        roots.push(h.join("Documents").join("lmms").join("samples"));
    }
    for sys in ["/usr/share/lmms/samples", "/usr/local/share/lmms/samples", "C:/Program Files/LMMS/data/samples"] {
        roots.push(sys.into());
    }
    roots.into_iter().map(|r| r.join(rel)).find(|p| p.is_file()).map(|p| p.to_string_lossy().into_owned())
}

/// The closest Ponzik instrument for an LMMS instrument (plugin name, its
/// settings element, the track's name), and its sample when it plays one.
fn instrument_for(instrument: Option<&Element>, track_name: &str) -> (&'static str, Option<String>) {
    let Some(inst) = instrument else { return (instrument_for_name(track_name), None) };
    let plugin = inst.attr("name").unwrap_or("").to_ascii_lowercase();
    let settings = inst.children.first();
    let src = settings.and_then(|s| s.attr("src")).filter(|s| !s.is_empty()).map(str::to_string);
    let kind = match plugin.as_str() {
        "audiofileprocessor" => return ("sampler", src),
        "slicert" => return ("slicer", src),
        "kicker" => "kick",
        "lb302" | "lb303" => "bass303",
        "organic" => "organ",
        "vibedstrings" | "vibed" => "guitar",
        "malletsstk" => "mallets",
        "monstro" | "watsyn" | "bitinvader" => "wavetable",
        "opulenz" => "fm",
        "sfxr" | "papu" | "freeboy" | "nes" | "xpressive" => "pulse",
        "gigplayer" | "patman" => "piano",
        "sf2player" => {
            let s = settings;
            let bank = s.and_then(|s| num(s, "bank")).unwrap_or(0.0);
            let patch = s.and_then(|s| num(s, "patch")).unwrap_or(0.0).clamp(0.0, 127.0) as u8;
            instrument_for_program(patch, if bank >= 128.0 { 9 } else { 0 })
        }
        "vestige" => {
            let plug = settings.and_then(|s| s.attr("plugin")).unwrap_or(track_name);
            instrument_for_name(crate::flp::file_name_of(plug))
        }
        _ => instrument_for_name(track_name),
    };
    (kind, None)
}

/// The notes of a pattern (Ponzik ticks from its start) and its length.
fn read_pattern(pat: &Element, base_note: i32) -> (Vec<Note>, u32) {
    let mut notes = Vec::new();
    let mut end = 0u32;
    for n in pat.children_named("note") {
        let (Some(pos), Some(key)) = (num(n, "pos"), num(n, "key")) else { continue };
        let len = num(n, "len").unwrap_or(48.0);
        let start = tick(pos);
        // A negative length: a step of the beat editor, played whole.
        let len = if len <= 0.0 { TICKS_PER_STEP } else { tick(len).max(1) };
        let midi = (key as i32 + (69 - base_note)).clamp(0, 127) as u8;
        let vol = num(n, "vol").unwrap_or(100.0);
        let pan = num(n, "pan").unwrap_or(0.0);
        if start >= MAX_TICK {
            continue;
        }
        end = end.max(start.saturating_add(len));
        notes.push(Note {
            start,
            len,
            key: midi,
            vel: (vol / 125.0).clamp(0.0, 1.0) as f32,
            pan: (pan / 100.0).clamp(-1.0, 1.0) as f32,
            ..Note::DEFAULT
        });
    }
    let bar = tick(LMMS_BAR);
    let len = match num(pat, "len").filter(|l| *l > 0.0) {
        Some(l) => tick(l),
        // A beat pattern lasts its steps; a melody, its notes to the bar.
        None if num(pat, "type") == Some(0.0) => tick(num(pat, "steps").unwrap_or(16.0).max(1.0) * 12.0),
        None => end.div_ceil(bar).max(1) * bar,
    };
    (notes, len.max(1))
}

/// Converts an LMMS project (`.mmp` or `.mmpz`) into a new Ponzik project.
/// `resolve` turns a sample path of the project into a readable path (see
/// [`resolve_lmms_sample`]).
pub fn import_lmms(bytes: &[u8], name: &str, resolve: &dyn Fn(&str) -> Option<String>) -> Result<(Project, ImportReport), String> {
    let plain = bytes.iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'<');
    let xml = if plain {
        bytes.to_vec()
    } else {
        crate::gzip::qt_uncompress(bytes, MAX_FILE).map_err(|e| tr_fmt!("Not a readable LMMS project: {}", e))?
    };
    let text = String::from_utf8_lossy(&xml);
    let root = parse_xml(&text)?;
    if root.name != "lmms-project" && root.name != "multimedia-project" {
        return Err(tr("Not an LMMS project").into());
    }
    let mut b = Build::new("LMMS", name);
    if let Some(v) = root.attr("creatorversion") {
        b.report.mapped.push(tr_fmt!("Saved by {}", format!("LMMS {v}")));
    }
    let head = root.child("head");
    let bpm = head.and_then(|h| num(h, "bpm").or_else(|| h.child("bpm").and_then(|e| num(e, "value")))).unwrap_or(120.0);
    b.tempo(bpm, &[]);
    let sig_num = head.and_then(|h| num(h, "timesig_numerator")).unwrap_or(4.0);
    let sig_den = head.and_then(|h| num(h, "timesig_denominator")).unwrap_or(4.0);
    b.signature(0, sig_num as u32, sig_den as u32);
    let secs_per_tick = 60.0 / b.p.bpm as f64 / LMMS_PPQ;

    let song = root.child("song");
    // FX mixer: channel n becomes mixer track n (when they come in order).
    let mixer = song.and_then(|s| child_of(s, &["fxmixer", "mixer"]));
    let mut fx_tracks: Vec<(usize, usize)> = Vec::new();
    let mut fx_effects = 0;
    if let Some(m) = mixer {
        for ch in children_of(m, &["fxchannel", "mixerchannel"]) {
            let n = num(ch, "num").unwrap_or(0.0) as usize;
            let vol = num(ch, "volume").unwrap_or(1.0) as f32;
            let muted = num(ch, "muted").unwrap_or(0.0) > 0.5;
            fx_effects += ch.child("fxchain").map_or(0, |c| c.children_named("effect").count());
            if n == 0 {
                let master_vol = head.and_then(|h| num(h, "mastervol")).unwrap_or(100.0) as f32 / 100.0;
                b.p.tracks[MASTER].gain = finite(vol * master_vol, 1.0).clamp(0.0, 4.0);
                continue;
            }
            let label = ch.attr("name").filter(|s| !s.trim().is_empty()).map_or_else(|| tr_fmt!("FX {}", n), str::to_string);
            let slot = b.track(&label, 0, vol, 0.0, muted);
            fx_tracks.push((n, slot));
        }
        // Routing: the first send is the output, the others are sends.
        let mut sends = 0;
        for ch in children_of(m, &["fxchannel", "mixerchannel"]) {
            let n = num(ch, "num").unwrap_or(0.0) as usize;
            let Some(&(_, from)) = fx_tracks.iter().find(|f| f.0 == n) else { continue };
            for (k, s) in ch.children_named("send").enumerate() {
                let to_n = num(s, "channel").unwrap_or(0.0) as usize;
                let to = if to_n == 0 { MASTER } else { fx_tracks.iter().find(|f| f.0 == to_n).map_or(MASTER, |f| f.1) };
                let amount = num(s, "amount").unwrap_or(1.0) as f32;
                if k == 0 {
                    if to != MASTER {
                        route(&mut b.p, from, to);
                    }
                } else if let Some(slot) = b.p.tracks[from].sends.iter_mut().find(|s| s.0 == MASTER && s.1 == 0.0) {
                    *slot = (to, finite(amount, 1.0).clamp(0.0, 4.0));
                    sends += 1;
                }
            }
        }
        if !fx_tracks.is_empty() {
            b.report.mapped.push(tr_fmt!("FX mixer: {} channels with their name, volume, mute and routing", fx_tracks.len()));
        }
        if sends > 0 {
            b.report.mapped.push(tr_fmt!("{} sends", sends));
        }
    }
    // The mixer track of an LMMS track: its FX channel, or one of its own.
    let fx_of = |b: &mut Build, fxch: usize, label: &str, color: u32| -> usize {
        match fx_tracks.iter().find(|f| f.0 == fxch && fxch > 0) {
            Some(f) => f.1,
            None => b.track(label, color, 1.0, 0.0, false),
        }
    };

    let container = song.and_then(|s| s.children_named("trackcontainer").next());
    let tracks: Vec<&Element> = container.map_or_else(Vec::new, |c| c.children_named("track").collect());
    // The Beat/Bassline editor: in the first Beat/Bassline track.
    let bb_container = tracks.iter().filter(|t| t.attr("type") == Some("1")).find_map(|t| {
        child_of(t, &["bbtrack", "patterntrack"]).and_then(|bt| bt.children_named("trackcontainer").next())
    });
    let (mut automation, mut skipped_types) = (0, 0);

    // Channels of the beat editor and their patterns, by Beat/Bassline.
    let mut beat_patterns: Vec<Vec<(usize, Vec<Note>, u32)>> = Vec::new();
    let add_instrument = |b: &mut Build, t: &Element, label: &str| -> Option<(usize, i32)> {
        let it = t.child("instrumenttrack")?;
        let color = t.attr("color").map_or(0, hex_color);
        let (kind, sample) = instrument_for(it.child("instrument"), label);
        let mut def = InstrumentDef::new(kind);
        if let Some(raw) = sample {
            let relative = !raw.starts_with('/') && !raw.contains(':');
            def.sample = Some(b.locate(&raw, resolve, relative.then_some("LMMS")));
        }
        let fxch = num(it, "fxch").or_else(|| num(it, "mixch")).unwrap_or(0.0).max(0.0) as usize;
        let track = fx_of(b, fxch, label, color);
        let ch = b.channel(label, def, track, color);
        let c = &mut b.p.channels[ch];
        c.gain = (num(it, "vol").unwrap_or(100.0) / 100.0).clamp(0.0, 2.0) as f32;
        c.pan = (num(it, "pan").unwrap_or(0.0) / 100.0).clamp(-1.0, 1.0) as f32;
        c.muted = t.attr("muted") == Some("1");
        let base = num(it, "basenote").map_or(LMMS_BASE, |v| v as i32);
        c.root_note = (60 + 69 - base).clamp(0, 127) as u8;
        Some((ch, base))
    };
    if let Some(bbc) = bb_container {
        for (k, t) in bbc.children_named("track").enumerate() {
            let label = t.attr("name").filter(|s| !s.trim().is_empty()).map_or_else(|| tr_fmt!("Track {}", k + 1), str::to_string);
            let Some((ch, base)) = add_instrument(&mut b, t, &label) else { continue };
            let pats: Vec<&Element> = children_of(t, PATTERN).collect();
            let by_bar = pats.iter().all(|p| num(p, "pos").is_some_and(|v| v % LMMS_BAR == 0.0));
            for (i, pat) in pats.iter().enumerate() {
                let idx = if by_bar { (num(pat, "pos").unwrap_or(0.0) / LMMS_BAR) as usize } else { i };
                if idx > 4096 {
                    continue;
                }
                if beat_patterns.len() <= idx {
                    beat_patterns.resize(idx + 1, Vec::new());
                }
                let (notes, len) = read_pattern(pat, base);
                beat_patterns[idx].push((ch, notes, len));
            }
        }
    }

    let mut bb_index = 0;
    let mut bb_made: Vec<Option<usize>> = Vec::new();
    for (lane, t) in tracks.iter().enumerate() {
        let label = t.attr("name").filter(|s| !s.trim().is_empty()).map_or_else(|| tr_fmt!("Track {}", lane + 1), str::to_string);
        let muted_track = t.attr("muted") == Some("1");
        match t.attr("type").unwrap_or("0") {
            "0" => {
                let Some((ch, base)) = add_instrument(&mut b, t, &label) else { continue };
                let color = b.p.channels[ch].color;
                for pat in children_of(t, PATTERN) {
                    let (notes, len) = read_pattern(pat, base);
                    let name = pat.attr("name").filter(|s| !s.trim().is_empty()).unwrap_or(&label).to_string();
                    let color = pat.attr("color").map_or(color, hex_color);
                    let pattern = b.pattern(ch, notes, len, &name, color);
                    let start = tick(num(pat, "pos").unwrap_or(0.0));
                    b.clip(pattern, lane, start, len, 0, pat.attr("muted") == Some("1"));
                }
            }
            "1" => {
                let k = bb_index;
                bb_index += 1;
                if bb_made.len() <= k {
                    bb_made.resize(k + 1, None);
                }
                for clip in children_of(t, BB_CLIP) {
                    let pattern = *bb_made[k].get_or_insert_with(|| {
                        let parts = beat_patterns.get(k).cloned().unwrap_or_default();
                        let len = parts.iter().map(|p| p.2).max().unwrap_or(tick(LMMS_BAR));
                        let color = t.attr("color").or_else(|| clip.attr("color")).map_or(0, hex_color);
                        b.multi_pattern(parts.into_iter().map(|(ch, n, _)| (ch, n)).collect(), len, &label, color)
                    });
                    let start = tick(num(clip, "pos").unwrap_or(0.0));
                    let len = tick(num(clip, "len").unwrap_or(LMMS_BAR)).max(1);
                    b.clip(pattern, lane, start, len, 0, muted_track || clip.attr("muted") == Some("1"));
                }
            }
            "2" => {
                let st = child_of(t, &["sampletrack"]);
                let color = t.attr("color").map_or(0, hex_color);
                let fxch = st.and_then(|s| num(s, "fxch").or_else(|| num(s, "mixch"))).unwrap_or(0.0).max(0.0) as usize;
                let track = fx_of(&mut b, fxch, &label, color);
                if let Some(st) = st.filter(|_| track != MASTER) {
                    let tr = &mut b.p.tracks[track];
                    if fxch == 0 {
                        tr.gain = (num(st, "vol").unwrap_or(100.0) / 100.0).clamp(0.0, 2.0) as f32;
                        tr.pan = (num(st, "pan").unwrap_or(0.0) / 100.0).clamp(-1.0, 1.0) as f32;
                        tr.muted = muted_track;
                    }
                }
                for clip in children_of(t, SAMPLE_CLIP) {
                    let Some(raw) = clip.attr("src").filter(|s| !s.is_empty()) else { continue };
                    let relative = !raw.starts_with('/') && !raw.contains(':');
                    let path = b.locate(raw, resolve, relative.then_some("LMMS"));
                    let (mut pos, mut len) = (num(clip, "pos").unwrap_or(0.0).max(0.0), num(clip, "len").unwrap_or(LMMS_BAR));
                    let off = num(clip, "off").unwrap_or(0.0);
                    // A negative offset: the file starts before the clip (its left edge was cut).
                    let offset = if off < 0.0 { -off * secs_per_tick } else { 0.0 };
                    if off > 0.0 {
                        pos += off;
                        len -= off;
                    }
                    b.audio(AudioClipDef {
                        path,
                        lane,
                        track,
                        start: tick(pos),
                        len: tick(len.max(1.0)).max(1),
                        offset,
                        muted: muted_track || clip.attr("muted") == Some("1"),
                        reverse: clip.attr("reversed") == Some("1"),
                        color: clip.attr("color").map_or(0, hex_color),
                        ..Default::default()
                    });
                }
            }
            "5" | "6" => automation += 1,
            _ => skipped_types += 1,
        }
    }
    if bb_index > 0 {
        b.report.mapped.push(tr_fmt!("{} Beat/Bassline patterns with their playlist clips", bb_made.iter().flatten().count()));
    }
    if let Some(tl) = song.and_then(|s| s.child("timeline")) {
        let (a, z) = (num(tl, "lp0pos").unwrap_or(0.0), num(tl, "lp1pos").unwrap_or(0.0));
        if num(tl, "lpstate").unwrap_or(0.0) > 0.5 && z > a {
            b.p.loop_range = Some((tick(a), tick(z)));
            b.report.mapped.push(tr("Loop range").into());
        }
    }
    if automation > 0 {
        b.report.skipped.push(tr_fmt!("{} automation tracks", automation));
    }
    let track_effects: usize = tracks
        .iter()
        .chain(bb_container.map_or_else(Vec::new, |c| c.children_named("track").collect()).iter())
        .filter_map(|t| t.child("instrumenttrack").and_then(|i| i.child("fxchain")))
        .map(|c| c.children_named("effect").count())
        .sum();
    if fx_effects + track_effects > 0 {
        b.report.skipped.push(tr_fmt!("{} effect plugins (not converted)", fx_effects + track_effects));
    }
    if skipped_types > 0 {
        b.report.skipped.push(tr_fmt!("{} tracks of an unknown kind", skipped_types));
    }
    b.report.approximated.push(tr("Instrument settings, envelopes, arpeggios and chords are not converted").into());
    Ok(b.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ponzik_engine::{PlayMode, TICKS_PER_BAR, TimeSig};

    /// An LMMS 1.2 project written by hand like LMMS writes them.
    const PROJECT: &str = r##"<?xml version="1.0"?>
<!DOCTYPE lmms-project>
<lmms-project version="1.0" creator="LMMS" creatorversion="1.2.2" type="song">
  <head timesig_numerator="6" mastervol="80" timesig_denominator="8" bpm="140" masterpitch="0"/>
  <song>
    <trackcontainer width="600" x="5" y="5" maximized="0" height="300" visible="1" type="song" minimized="0">
      <track muted="0" type="0" name="Lead &amp; Co" solo="0" color="#ff8000">
        <instrumenttrack pan="-50" fxch="1" pitchrange="1" pitch="0" basenote="57" vol="50">
          <instrument name="tripleoscillator">
            <tripleoscillator vol0="33" />
          </instrument>
          <eldata fres="0.5" ftype="0" fcut="14000" fwet="0"/>
          <chordcreator chord-enabled="0"/>
          <arpeggiator arp-enabled="0"/>
          <midiport inputchannel="0" outputchannel="1"/>
          <fxchain numofeffects="1" enabled="1"><effect name="ladspaeffect" on="1"/></fxchain>
        </instrumenttrack>
        <pattern muted="0" type="1" name="Hook" pos="192" steps="16">
          <note pan="0" key="57" vol="100" pos="0" len="48"/>
          <note pan="50" key="60" vol="50" pos="96" len="24"/>
        </pattern>
        <pattern muted="1" type="1" name="" pos="768" steps="16">
          <note pan="0" key="57" vol="100" pos="0" len="48"/>
          <note pan="50" key="60" vol="50" pos="96" len="24"/>
        </pattern>
      </track>
      <track muted="0" type="1" name="Beat/Bassline 0" solo="0">
        <bbtrack>
          <trackcontainer width="640" type="bbtrackcontainer">
            <track muted="0" type="0" name="Kicker" solo="0">
              <instrumenttrack pan="0" fxch="0" basenote="57" vol="100">
                <instrument name="kicker"><kicker startfreq="150"/></instrument>
              </instrumenttrack>
              <pattern muted="0" type="0" name="" pos="0" steps="16">
                <note pan="0" key="57" vol="100" pos="0" len="-192"/>
                <note pan="0" key="57" vol="100" pos="48" len="-192"/>
              </pattern>
              <pattern muted="0" type="0" name="" pos="192" steps="32">
                <note pan="0" key="57" vol="100" pos="0" len="-192"/>
              </pattern>
            </track>
            <track muted="1" type="0" name="Hat" solo="0">
              <instrumenttrack pan="0" fxch="0" basenote="57" vol="100">
                <instrument name="audiofileprocessor"><audiofileprocessor src="drums/hihat_closed01.ogg" reversed="0"/></instrument>
              </instrumenttrack>
              <pattern muted="0" type="0" name="" pos="0" steps="16">
                <note pan="0" key="57" vol="100" pos="24" len="-192"/>
              </pattern>
            </track>
          </trackcontainer>
        </bbtrack>
        <bbtco muted="0" pos="0" len="384" name=""/>
        <bbtco muted="0" pos="768" len="192" name=""/>
      </track>
      <track muted="0" type="1" name="Fill" solo="0">
        <bbtrack/>
        <bbtco muted="0" pos="384" len="192" name=""/>
      </track>
      <track muted="1" type="2" name="Vocals" solo="0">
        <sampletrack vol="50" pan="25" fxch="0"/>
        <sampletco muted="0" pos="192" len="384" src="/music/vox.wav" off="-96"/>
      </track>
      <track muted="0" type="5" name="Automation track"><automationtrack/></track>
    </trackcontainer>
    <timeline lp1pos="768" lp0pos="192" lpstate="1"/>
    <fxmixer>
      <fxchannel num="0" name="Master" volume="1" muted="0"><fxchain numofeffects="0" enabled="0"/></fxchannel>
      <fxchannel num="1" name="Synths" volume="0.5" muted="0">
        <fxchain numofeffects="0" enabled="0"/>
        <send channel="2" amount="1"/>
        <send channel="0" amount="0.25"/>
      </fxchannel>
      <fxchannel num="2" name="Bus" volume="1" muted="1"><fxchain numofeffects="0" enabled="0"/><send channel="0" amount="1"/></fxchannel>
    </fxmixer>
  </song>
</lmms-project>
"##;

    fn resolve(raw: &str) -> Option<String> {
        raw.starts_with('/').then(|| raw.to_string())
    }

    #[test]
    fn an_lmms_project_imports() {
        let (p, report) = import_lmms(PROJECT.as_bytes(), "Song", &resolve).unwrap();
        assert_eq!(report.source, "LMMS");
        assert_eq!(p.bpm, 140.0);
        assert_eq!(p.signatures, vec![TimeSig { tick: 0, num: 6, den: 8 }]);
        assert_eq!(p.loop_range, Some((TICKS_PER_BAR, TICKS_PER_BAR * 4)));
        assert!((p.tracks[MASTER].gain - 0.8).abs() < 1e-6);
        // FX 1 → track 1 → FX 2 (track 2), with a send to the master.
        assert_eq!((p.tracks[1].name.as_str(), p.tracks[1].gain, p.tracks[1].output), ("Synths", 0.5, 2));
        assert_eq!(p.tracks[1].sends[0], (MASTER, 0.25));
        assert_eq!((p.tracks[2].name.as_str(), p.tracks[2].muted), ("Bus", true));
        // Channels: the beat channels, then the lead.
        let names: Vec<&str> = p.channels.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["Kicker", "Hat", "Lead & Co"]);
        let lead = &p.channels[2];
        assert_eq!((lead.instrument.kind.as_str(), lead.track, lead.color), ("pulse", 1, 0xff8000));
        assert!((lead.gain - 0.5).abs() < 1e-6 && (lead.pan + 0.5).abs() < 1e-6);
        assert_eq!(p.channels[0].instrument.kind, "kick");
        let hat = &p.channels[1];
        assert!(hat.muted);
        assert_eq!(hat.instrument.kind, "sampler");
        // The factory sample is not on this computer: a library sound or offline.
        assert!(hat.instrument.sample.is_some());
        // The lead's two clips share their pattern; the second is muted.
        let lead_clips: Vec<_> = p.clips.iter().filter(|c| c.lane == 0).collect();
        assert_eq!(lead_clips.len(), 2);
        assert_eq!(lead_clips[0].pattern, lead_clips[1].pattern);
        assert_eq!((lead_clips[0].start, lead_clips[1].start, lead_clips[1].muted), (2 * PPQ * 2, 16 * PPQ, true));
        let hook = &p.patterns[lead_clips[0].pattern];
        assert_eq!((hook.name.as_str(), hook.len), ("Hook", TICKS_PER_BAR));
        // LMMS key 57 is A4 (MIDI 69); volume 100 is 0.8.
        let n = &hook.notes[2];
        assert_eq!(n.iter().map(|n| (n.start, n.len, n.key)).collect::<Vec<_>>(), [(0, PPQ, 69), (2 * PPQ, PPQ / 2, 72)]);
        assert!((n[0].vel - 0.8).abs() < 1e-6 && (n[1].pan - 0.5).abs() < 1e-6);
        // Beat/Bassline 0: one pattern of the kick and the hat, its two clips.
        let bb: Vec<_> = p.clips.iter().filter(|c| c.lane == 1).collect();
        assert_eq!(bb.len(), 2);
        assert_eq!(bb[0].pattern, bb[1].pattern);
        assert_eq!((bb[0].start, bb[0].len, bb[1].start), (0, TICKS_PER_BAR * 2, TICKS_PER_BAR * 4));
        let beat = &p.patterns[bb[0].pattern];
        assert_eq!(beat.len, TICKS_PER_BAR);
        assert_eq!(beat.notes[0].iter().map(|n| (n.start, n.len)).collect::<Vec<_>>(), [(0, TICKS_PER_STEP), (PPQ, TICKS_PER_STEP)]);
        assert_eq!(beat.notes[1].iter().map(|n| n.start).collect::<Vec<_>>(), [PPQ / 2]);
        // "Fill" plays the kick's second pattern: 32 steps, two bars.
        let fill = p.clips.iter().find(|c| c.lane == 2).unwrap();
        assert_eq!((fill.start, p.patterns[fill.pattern].len), (TICKS_PER_BAR * 2, TICKS_PER_BAR * 2));
        assert_eq!(p.patterns[fill.pattern].notes[0].len(), 1);
        // The vocals: 96 LMMS ticks (2 beats) into the file.
        assert_eq!(p.audio_clips.len(), 1);
        let a = &p.audio_clips[0];
        assert_eq!((a.path.as_str(), a.start, a.len, a.lane), ("/music/vox.wav", TICKS_PER_BAR, TICKS_PER_BAR * 2, 3));
        assert!((a.offset - 2.0 * 60.0 / 140.0).abs() < 1e-9);
        assert!(a.muted);
        let vt = &p.tracks[a.track];
        assert_eq!((vt.name.as_str(), vt.gain, vt.pan), ("Vocals", 0.5, 0.25));
        assert_eq!(p.mode, PlayMode::Song);
        assert!(report.skipped.iter().any(|s| s.contains("1 automation tracks")));
        assert!(report.skipped.iter().any(|s| s.contains("1 effect plugins")));
    }

    #[test]
    fn mmpz_is_the_same_project_compressed() {
        let mut z = (PROJECT.len() as u32).to_be_bytes().to_vec();
        z.extend_from_slice(&crate::gzip::zlib_compress(PROJECT.as_bytes()));
        let (a, _) = import_lmms(&z, "Song", &resolve).unwrap();
        let (b, _) = import_lmms(PROJECT.as_bytes(), "Song", &resolve).unwrap();
        assert_eq!(a.clips, b.clips);
        assert_eq!(a.patterns, b.patterns);
    }

    #[test]
    fn junk_is_an_error_never_a_panic() {
        assert!(import_lmms(b"", "x", &resolve).is_err());
        assert!(import_lmms(&[0, 0, 0, 9, 0x78, 0x9c, 1, 2], "x", &resolve).is_err());
        assert!(import_lmms(b"<other/>", "x", &resolve).is_err());
        let (p, _) = import_lmms(b"<lmms-project/>", "x", &resolve).unwrap();
        assert!(p.channels.is_empty());
        for cut in (0..PROJECT.len()).step_by(11) {
            let _ = import_lmms(&PROJECT.as_bytes()[..cut], "x", &resolve);
        }
        let junk = r#"<lmms-project><head bpm="nan" timesig_numerator="0"/><song><trackcontainer>
            <track type="0"><instrumenttrack vol="inf" basenote="-99999" fxch="99999"/><pattern pos="-5" type="1" len="1e300"><note key="9999" pos="1e300" len="nan"/><note key="1" pos="0" len="1e30"/></pattern></track>
            <track type="1"><bbtco pos="1e300" len="-1"/></track><track type="2"><sampletco src="a.wav" off="1e300" len="-3"/></track>
            </trackcontainer></song></lmms-project>"#;
        let (p, _) = import_lmms(junk.as_bytes(), "x", &resolve).unwrap();
        assert!(p.bpm.is_finite() && p.channels.iter().all(|c| c.gain.is_finite()));
    }
}
