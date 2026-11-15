//! Fichiers MIDI standard (SMF) : lecture des formats 0 et 1, import dans un
//! pattern, export d'un pattern en format 1.

use std::collections::VecDeque;

use ponzik_engine::{Note, PPQ, TICKS_PER_BAR};

use crate::{InstrumentDef, PatternDef, Project};

/// Canal MIDI de la batterie (canal 10, numéroté 9 à partir de 0).
pub const DRUM_CHANNEL: u8 = 9;
/// Longueur maximale d'un pattern importé, en mesures.
pub const MAX_IMPORT_BARS: u32 = 64;

/// Contenu musical d'un fichier MIDI, en ticks Ponzik (`PPQ` par noire).
#[derive(Clone, Debug, PartialEq)]
pub struct MidiFile {
    /// Premier tempo du fichier, en BPM (120 par défaut).
    pub tempo: f32,
    /// Première signature rythmique (numérateur, dénominateur), 4/4 par défaut.
    pub time_signature: (u8, u8),
    pub tracks: Vec<MidiTrack>,
}

/// Les notes d'un canal MIDI d'une piste.
#[derive(Clone, Debug, PartialEq)]
pub struct MidiTrack {
    pub name: String,
    /// Canal MIDI, à partir de 0.
    pub channel: u8,
    /// Vrai pour le canal 10 (batterie General MIDI).
    pub drums: bool,
    /// Triées par (début, hauteur).
    pub notes: Vec<Note>,
}

// --- Lecture ---

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.pos.checked_add(n).filter(|&e| e <= self.bytes.len()).ok_or("fichier MIDI tronqué")?;
        let s = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, String> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// Quantité de longueur variable (4 octets au plus).
    fn vlq(&mut self) -> Result<u32, String> {
        let mut v = 0u32;
        for _ in 0..4 {
            let b = self.u8()?;
            v = (v << 7) | u32::from(b & 0x7f);
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err("quantité de longueur variable invalide".into())
    }

    fn done(&self) -> bool {
        self.pos >= self.bytes.len()
    }
}

/// Note brute d'une piste, en ticks du fichier.
struct RawNote {
    channel: u8,
    start: u64,
    end: u64,
    key: u8,
    vel: u8,
}

struct RawTrack {
    name: String,
    notes: Vec<RawNote>,
}

/// Lit une piste ; `tempo` reçoit le premier changement de tempo (tick, µs par noire).
fn read_track(
    data: &[u8],
    tempo: &mut Option<(u64, u32)>,
    sig: &mut Option<(u64, (u8, u8))>,
) -> Result<RawTrack, String> {
    let mut r = Reader { bytes: data, pos: 0 };
    let mut track = RawTrack { name: String::new(), notes: Vec::new() };
    // Notes ouvertes par (canal, hauteur) : début et vélocité, fermées dans l'ordre (FIFO).
    let mut open: Vec<VecDeque<(u64, u8)>> = (0..16 * 128).map(|_| VecDeque::new()).collect();
    let mut tick = 0u64;
    let mut running: Option<u8> = None;
    while !r.done() {
        tick += u64::from(r.vlq()?);
        let mut status = r.u8()?;
        let first_data = if status < 0x80 {
            let data = status;
            status = running.ok_or("octet de données sans statut (running status absent)")?;
            Some(data)
        } else {
            None
        };
        match status {
            0xff => {
                running = None;
                let kind = r.u8()?;
                let len = r.vlq()? as usize;
                let body = r.take(len)?;
                match kind {
                    0x2f => break,
                    0x03 if track.name.is_empty() => track.name = String::from_utf8_lossy(body).trim().to_string(),
                    0x51 if body.len() == 3 => {
                        let us = u32::from_be_bytes([0, body[0], body[1], body[2]]);
                        if us > 0 && tempo.is_none_or(|(t, _)| tick < t) {
                            *tempo = Some((tick, us));
                        }
                    }
                    0x58 if body.len() >= 2 => {
                        let ts = (body[0], 1u8.checked_shl(u32::from(body[1])).unwrap_or(4));
                        if sig.is_none_or(|(t, _)| tick < t) {
                            *sig = Some((tick, ts));
                        }
                    }
                    _ => {}
                }
            }
            0xf0 | 0xf7 => {
                running = None;
                let len = r.vlq()? as usize;
                r.take(len)?;
            }
            0xf1..=0xfe => return Err(format!("message système inattendu dans une piste : {status:#04x}")),
            _ => {
                running = Some(status);
                let a = match first_data {
                    Some(d) => d,
                    None => r.u8()?,
                };
                let kind = status & 0xf0;
                let b = if matches!(kind, 0xc0 | 0xd0) { 0 } else { r.u8()? };
                let channel = status & 0x0f;
                let (key, vel) = (a & 0x7f, b & 0x7f);
                let slot = &mut open[usize::from(channel) * 128 + usize::from(key)];
                match kind {
                    0x90 if vel > 0 => slot.push_back((tick, vel)),
                    0x80 | 0x90 => {
                        if let Some((start, vel)) = slot.pop_front() {
                            track.notes.push(RawNote { channel, start, end: tick, key, vel });
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    // Notes jamais relâchées : fermées en fin de piste.
    for (i, slot) in open.iter_mut().enumerate() {
        for (start, vel) in slot.drain(..) {
            let end = tick.max(start + 1);
            track.notes.push(RawNote { channel: (i / 128) as u8, start, end, key: (i % 128) as u8, vel });
        }
    }
    Ok(track)
}

/// Lit un fichier MIDI standard (formats 0 et 1, division en ticks par noire).
pub fn read_smf(bytes: &[u8]) -> Result<MidiFile, String> {
    let mut r = Reader { bytes, pos: 0 };
    if r.take(4).ok() != Some(b"MThd".as_slice()) {
        return Err("ce n'est pas un fichier MIDI (en-tête MThd absent)".into());
    }
    let header_len = r.u32()? as usize;
    if header_len < 6 {
        return Err("en-tête MIDI trop court".into());
    }
    let header = r.take(header_len)?;
    let format = u16::from_be_bytes([header[0], header[1]]);
    let division = u16::from_be_bytes([header[4], header[5]]);
    if format > 1 {
        return Err(format!("format MIDI {format} non pris en charge (seuls les formats 0 et 1 le sont)"));
    }
    if division & 0x8000 != 0 {
        return Err("division temporelle SMPTE non prise en charge (ticks par noire requis)".into());
    }
    if division == 0 {
        return Err("division temporelle nulle".into());
    }
    let mut tempo = None;
    let mut sig = None;
    let mut raw = Vec::new();
    while !r.done() {
        let id = r.take(4)?;
        let len = r.u32()? as usize;
        let data = r.take(len)?;
        if id == b"MTrk" {
            raw.push(read_track(data, &mut tempo, &mut sig)?);
        }
    }

    let div = u64::from(division);
    let scale = |t: u64| ((t * u64::from(PPQ) + div / 2) / div).min(u64::from(u32::MAX)) as u32;
    let mut tracks = Vec::new();
    for (ti, t) in raw.into_iter().enumerate() {
        let mut channels: Vec<u8> = t.notes.iter().map(|n| n.channel).collect();
        channels.sort_unstable();
        channels.dedup();
        let several = channels.len() > 1;
        for ch in channels {
            let mut notes: Vec<Note> = t
                .notes
                .iter()
                .filter(|n| n.channel == ch)
                .map(|n| {
                    let start = scale(n.start);
                    let len = scale(n.end).saturating_sub(start).max(1);
                    Note { start, len, key: n.key, vel: f32::from(n.vel) / 127.0 }
                })
                .collect();
            notes.sort_by_key(|n| (n.start, n.key));
            let name = match (t.name.is_empty(), several) {
                (true, _) => format!("Piste {} (canal {})", ti + 1, ch + 1),
                (false, true) => format!("{} (canal {})", t.name, ch + 1),
                (false, false) => t.name.clone(),
            };
            tracks.push(MidiTrack { name, channel: ch, drums: ch == DRUM_CHANNEL, notes });
        }
    }
    Ok(MidiFile {
        tempo: tempo.map_or(120.0, |(_, us)| (60_000_000.0 / f64::from(us)) as f32),
        time_signature: sig.map_or((4, 4), |(_, s)| s),
        tracks,
    })
}

// --- Import ---

/// Importe un fichier MIDI dans un nouveau pattern : chaque piste devient un
/// canal (Pulse, ou DrumSynth pour la batterie). Le pattern est arrondi à la
/// mesure et limité à `MAX_IMPORT_BARS` mesures. Le tempo est repris si le
/// projet n'a encore aucun canal. Renvoie l'indice du pattern créé.
pub fn import_midi(project: &mut Project, file: &MidiFile, name: &str) -> usize {
    if project.channels.is_empty() && file.tempo.is_finite() && file.tempo > 0.0 {
        project.bpm = file.tempo;
    }
    let end = file.tracks.iter().flat_map(|t| &t.notes).map(|n| n.start.saturating_add(n.len)).max().unwrap_or(0);
    let len = end.div_ceil(TICKS_PER_BAR).clamp(1, MAX_IMPORT_BARS) * TICKS_PER_BAR;
    project.patterns.push(PatternDef {
        name: name.into(),
        len,
        notes: vec![Vec::new(); project.channels.len()],
        color: 0,
    });
    let pattern = project.patterns.len() - 1;
    for t in &file.tracks {
        let kind = if t.drums { "drumsynth" } else { "pulse" };
        let root = t.notes.first().map_or(60, |n| n.key);
        let ch = project.add_channel(t.name.clone(), InstrumentDef::new(kind), root);
        project.patterns[pattern].notes[ch] = t
            .notes
            .iter()
            .filter(|n| n.start < len)
            .map(|n| Note { len: n.len.min(len - n.start).max(1), ..*n })
            .collect();
        project.sort_notes(pattern, ch);
    }
    pattern
}

// --- Export ---

fn put_vlq(out: &mut Vec<u8>, mut v: u32) {
    let mut buf = [0u8; 5];
    let mut i = buf.len() - 1;
    buf[i] = (v & 0x7f) as u8;
    v >>= 7;
    while v > 0 {
        i -= 1;
        buf[i] = (v & 0x7f) as u8 | 0x80;
        v >>= 7;
    }
    out.extend_from_slice(&buf[i..]);
}

fn put_meta(out: &mut Vec<u8>, kind: u8, data: &[u8]) {
    out.extend_from_slice(&[0xff, kind]);
    put_vlq(out, data.len() as u32);
    out.extend_from_slice(data);
}

fn put_chunk(out: &mut Vec<u8>, id: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(id);
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(data);
}

/// Canal MIDI d'un canal Ponzik : la batterie sur le canal 10, les autres
/// répartis sur les 15 canaux restants.
fn midi_channel(project: &Project, index: usize) -> u8 {
    if project.channels[index].instrument.kind == "drumsynth" {
        return DRUM_CHANNEL;
    }
    let c = (index % 15) as u8;
    if c >= DRUM_CHANNEL { c + 1 } else { c }
}

/// Exporte un pattern en fichier MIDI format 1 (division `PPQ`) : une piste de
/// tempo puis une piste par canal ayant des notes.
pub fn write_smf(project: &Project, pattern: usize) -> Vec<u8> {
    let mut tracks = Vec::new();

    let mut conductor = Vec::new();
    put_vlq(&mut conductor, 0);
    put_meta(&mut conductor, 0x03, project.name.as_bytes());
    put_vlq(&mut conductor, 0);
    let us = (60_000_000.0 / f64::from(project.bpm.max(1.0))).round().clamp(1.0, 16_777_215.0) as u32;
    put_meta(&mut conductor, 0x51, &us.to_be_bytes()[1..]);
    put_vlq(&mut conductor, 0);
    put_meta(&mut conductor, 0x58, &[4, 2, 24, 8]);
    put_vlq(&mut conductor, 0);
    put_meta(&mut conductor, 0x2f, &[]);
    tracks.push(conductor);

    let empty = Vec::new();
    let pat_notes = project.patterns.get(pattern).map_or(&empty, |p| &p.notes);
    for (i, notes) in pat_notes.iter().enumerate().take(project.channels.len()) {
        if notes.is_empty() {
            continue;
        }
        let ch = midi_channel(project, i);
        // (tick, 0 = relâchement / 1 = attaque, message) : relâchements d'abord à tick égal.
        let mut events: Vec<(u32, u8, [u8; 3])> = Vec::with_capacity(notes.len() * 2);
        for n in notes {
            let key = n.key.min(127);
            let vel = (n.vel * 127.0).round().clamp(1.0, 127.0) as u8;
            events.push((n.start, 1, [0x90 | ch, key, vel]));
            events.push((n.start.saturating_add(n.len.max(1)), 0, [0x80 | ch, key, 0]));
        }
        events.sort_by_key(|e| (e.0, e.1));
        let mut data = Vec::new();
        put_vlq(&mut data, 0);
        put_meta(&mut data, 0x03, project.channels[i].name.as_bytes());
        let mut last = 0;
        for (tick, _, msg) in events {
            put_vlq(&mut data, tick - last);
            data.extend_from_slice(&msg);
            last = tick;
        }
        put_vlq(&mut data, 0);
        put_meta(&mut data, 0x2f, &[]);
        tracks.push(data);
    }

    let mut out = Vec::new();
    let mut header = Vec::with_capacity(6);
    header.extend_from_slice(&1u16.to_be_bytes());
    header.extend_from_slice(&(tracks.len() as u16).to_be_bytes());
    header.extend_from_slice(&(PPQ as u16).to_be_bytes());
    put_chunk(&mut out, b"MThd", &header);
    for t in &tracks {
        put_chunk(&mut out, b"MTrk", t);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn smf(format: u16, division: u16, tracks: &[Vec<u8>]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut header = Vec::new();
        header.extend_from_slice(&format.to_be_bytes());
        header.extend_from_slice(&(tracks.len() as u16).to_be_bytes());
        header.extend_from_slice(&division.to_be_bytes());
        put_chunk(&mut out, b"MThd", &header);
        for t in tracks {
            put_chunk(&mut out, b"MTrk", t);
        }
        out
    }

    fn keys(t: &MidiTrack) -> Vec<(u32, u32, u8)> {
        t.notes.iter().map(|n| (n.start, n.len, n.key)).collect()
    }

    #[test]
    fn format0_running_status_split_by_channel() {
        #[rustfmt::skip]
        let track = vec![
            0x00, 0xff, 0x03, 0x04, b'D', b'e', b'm', b'o',
            0x00, 0xff, 0x51, 0x03, 0x07, 0xa1, 0x20, // 500 000 µs = 120 BPM
            0x00, 0xff, 0x58, 0x04, 0x03, 0x02, 0x18, 0x08, // 3/4
            0x00, 0xf0, 0x03, 0x7e, 0x09, 0xf7, // sysex ignoré
            0x00, 0x90, 60, 100,
            0x00, 64, 90, // running status
            0x30, 60, 0, // note on vel 0 = relâchement
            0x30, 64, 0,
            0x00, 0x99, 36, 127, // batterie
            0x18, 0x89, 36, 0,
            0x00, 0xff, 0x2f, 0x00,
        ];
        let f = read_smf(&smf(0, 96, &[track])).unwrap();
        assert_eq!(f.tempo, 120.0);
        assert_eq!(f.time_signature, (3, 4));
        assert_eq!(f.tracks.len(), 2);
        let (mel, drums) = (&f.tracks[0], &f.tracks[1]);
        assert_eq!((mel.channel, mel.drums, mel.name.as_str()), (0, false, "Demo (canal 1)"));
        assert_eq!(keys(mel), [(0, 48, 60), (0, 96, 64)]);
        assert!((mel.notes[0].vel - 100.0 / 127.0).abs() < 1e-6);
        assert_eq!((drums.channel, drums.drums), (9, true));
        assert_eq!(keys(drums), [(96, 24, 36)]);
    }

    #[test]
    fn format1_division_rescaled_and_fifo_overlaps() {
        #[rustfmt::skip]
        let conductor = vec![
            0x00, 0xff, 0x51, 0x03, 0x06, 0x1a, 0x80, // 400 000 µs = 150 BPM
            0x00, 0xff, 0x2f, 0x00,
        ];
        #[rustfmt::skip]
        let lead = vec![
            0x00, 0xff, 0x03, 0x04, b'L', b'e', b'a', b'd',
            0x00, 0x91, 60, 80,
            0x83, 0x60, 60, 80, // +480 : même hauteur, nouvelle note
            0x83, 0x60, 0x81, 60, 0, // +480 : ferme la première (FIFO)
            0x81, 0x70, 60, 0, // +240 : ferme la seconde
            0x00, 0xff, 0x2f, 0x00,
        ];
        let f = read_smf(&smf(1, 480, &[conductor, lead])).unwrap();
        assert!((f.tempo - 150.0).abs() < 1e-3);
        assert_eq!(f.tracks.len(), 1);
        let t = &f.tracks[0];
        assert_eq!((t.name.as_str(), t.channel), ("Lead", 1));
        // 480 ticks par noire -> 96 : premier 0..960 => 0..192, second 480..1200 => 96..240.
        assert_eq!(keys(t), [(0, 192, 60), (96, 144, 60)]);
    }

    #[test]
    fn invalid_files_rejected() {
        let err = read_smf(&smf(1, 0xe728, &[])).unwrap_err();
        assert!(err.contains("SMPTE"), "{err}");
        assert!(read_smf(b"RIFF....").is_err());
        assert!(read_smf(&smf(0, 96, &[vec![0x00, 0x40, 0x40]])).is_err(), "données sans statut");
        let mut truncated = smf(0, 96, &[vec![0x00, 0x90, 60, 100, 0x10, 0x80, 60, 0]]);
        truncated.truncate(truncated.len() - 3);
        assert!(read_smf(&truncated).is_err());
    }

    fn sample_project() -> Project {
        let mut p = Project::empty();
        p.bpm = 137.0;
        let bass = p.add_channel("Basse", InstrumentDef::new("pulse"), 36);
        let kick = p.add_channel("Kick", InstrumentDef::new("drumsynth"), 36);
        p.add_channel("Vide", InstrumentDef::new("pulse"), 60);
        for (start, len, key, vel) in [(0, 48, 36, 0.8), (48, 48, 36, 0.5), (96, 192, 43, 1.0), (96, 24, 48, 0.33)] {
            p.add_note(0, bass, Note { start, len, key, vel });
        }
        for step in [0, 4, 8, 12] {
            p.set_step(0, kick, step, true);
        }
        p
    }

    #[test]
    fn round_trip_preserves_notes_tempo_and_names() {
        let p = sample_project();
        let bytes = write_smf(&p, 0);
        assert_eq!(&bytes[..4], b"MThd");
        let f = read_smf(&bytes).unwrap();
        assert!((f.tempo - 137.0).abs() < 0.01, "{}", f.tempo);
        assert_eq!(f.tracks.len(), 2, "le canal vide n'est pas exporté");
        for (t, ch) in f.tracks.iter().zip([0, 1]) {
            let src = &p.patterns[0].notes[ch];
            assert_eq!(t.name, p.channels[ch].name);
            assert_eq!(t.notes.len(), src.len());
            for (a, b) in t.notes.iter().zip(src) {
                assert_eq!((a.start, a.len, a.key), (b.start, b.len, b.key));
                assert!((a.vel - b.vel).abs() <= 1.0 / 127.0, "{} {}", a.vel, b.vel);
            }
        }
        assert_eq!((f.tracks[0].channel, f.tracks[0].drums), (0, false));
        assert_eq!((f.tracks[1].channel, f.tracks[1].drums), (DRUM_CHANNEL, true));
    }

    #[test]
    fn note_off_before_note_on_at_equal_ticks() {
        let mut p = Project::empty();
        let ch = p.add_channel("Lead", InstrumentDef::new("pulse"), 60);
        p.add_note(0, ch, Note { start: 0, len: 96, key: 60, vel: 1.0 });
        p.add_note(0, ch, Note { start: 96, len: 96, key: 60, vel: 1.0 });
        let f = read_smf(&write_smf(&p, 0)).unwrap();
        assert_eq!(keys(&f.tracks[0]), [(0, 96, 60), (96, 96, 60)]);
    }

    #[test]
    fn melodic_channels_skip_the_drum_channel() {
        let mut p = Project::empty();
        for i in 0..17 {
            p.add_channel(format!("C{i}"), InstrumentDef::new("pulse"), 60);
        }
        let chans: Vec<u8> = (0..17).map(|i| midi_channel(&p, i)).collect();
        assert_eq!(chans, [0, 1, 2, 3, 4, 5, 6, 7, 8, 10, 11, 12, 13, 14, 15, 0, 1]);
    }

    #[test]
    fn import_creates_channels_and_pattern() {
        let f = read_smf(&write_smf(&sample_project(), 0)).unwrap();
        let mut p = Project::empty();
        let pat = import_midi(&mut p, &f, "Import");
        assert_eq!(pat, 1);
        assert!((p.bpm - 137.0).abs() < 0.01);
        assert_eq!(p.channels.len(), 2);
        assert_eq!(p.channels[0].instrument.kind, "pulse");
        assert_eq!(p.channels[1].instrument.kind, "drumsynth");
        assert_eq!(p.channels[1].name, "Kick");
        assert_eq!(p.patterns[pat].name, "Import");
        assert_eq!(p.patterns[pat].len, TICKS_PER_BAR, "notes jusqu'à 288 : une mesure");
        assert_eq!(p.patterns[pat].notes[0].len(), 4);
        assert_eq!(p.patterns[0].notes.len(), 2, "les autres patterns suivent les canaux");

        // Deuxième import : tempo du projet conservé, longueur arrondie et limitée.
        let long = MidiFile {
            tempo: 90.0,
            time_signature: (4, 4),
            tracks: vec![MidiTrack {
                name: "Long".into(),
                channel: 0,
                drums: false,
                notes: vec![
                    Note { start: 0, len: TICKS_PER_BAR + 1, key: 60, vel: 1.0 },
                    Note { start: TICKS_PER_BAR * 70, len: 96, key: 62, vel: 1.0 },
                ],
            }],
        };
        let pat2 = import_midi(&mut p, &long, "Long");
        assert!((p.bpm - 137.0).abs() < 0.01);
        assert_eq!(p.patterns[pat2].len, MAX_IMPORT_BARS * TICKS_PER_BAR);
        assert_eq!(p.patterns[pat2].notes[2].len(), 1, "note hors limite ignorée");
        let short = MidiFile {
            tracks: vec![MidiTrack { notes: vec![long.tracks[0].notes[0]], ..long.tracks[0].clone() }],
            ..long
        };
        let pat3 = import_midi(&mut p, &short, "Court");
        assert_eq!(p.patterns[pat3].len, 2 * TICKS_PER_BAR);
    }
}
