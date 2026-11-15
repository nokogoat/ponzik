//! Fichiers de projet `.ponzik` : JSON lisible, chargement tolérant.
//!
//! Les champs absents prennent leur valeur par défaut, les champs inconnus sont
//! ignorés et tous les index sont vérifiés : un fichier abîmé ou écrit par une
//! version plus récente (même majeure) se charge au mieux.

use std::path::Path;

use ponzik_engine::mixer::{INSERT_SLOTS, MASTER, SEND_SLOTS, TRACKS};
use ponzik_engine::{Note, PlayMode, TICKS_PER_BAR};

use crate::json::{self, Json};
use crate::{ChannelDef, ClipDef, EffectDef, InstrumentDef, PatternDef, Project, TrackDef};

pub const FORMAT: &str = "ponzik-project";
/// Version majeure du format : une version supérieure est refusée.
pub const VERSION: u32 = 1;

// --- Sauvegarde ---

fn obj(pairs: Vec<(&str, Json)>) -> Json {
    Json::Obj(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

fn params_json(params: &[(u32, f32)]) -> Json {
    Json::Arr(params.iter().map(|&(id, v)| Json::Arr(vec![id.into(), v.into()])).collect())
}

fn channel_json(c: &ChannelDef) -> Json {
    obj(vec![
        ("name", c.name.as_str().into()),
        (
            "instrument",
            obj(vec![
                ("kind", c.instrument.kind.as_str().into()),
                ("params", params_json(&c.instrument.params)),
                ("sample", c.instrument.sample.clone().into()),
            ]),
        ),
        ("gain", c.gain.into()),
        ("pan", c.pan.into()),
        ("muted", c.muted.into()),
        ("track", c.track.into()),
        ("root_note", c.root_note.into()),
        ("color", c.color.into()),
    ])
}

fn pattern_json(p: &PatternDef) -> Json {
    let notes = p
        .notes
        .iter()
        .map(|notes| {
            Json::Arr(
                notes
                    .iter()
                    .map(|n| Json::Arr(vec![n.start.into(), n.len.into(), n.key.into(), n.vel.into()]))
                    .collect(),
            )
        })
        .collect();
    obj(vec![
        ("name", p.name.as_str().into()),
        ("len", p.len.into()),
        ("color", p.color.into()),
        ("notes", Json::Arr(notes)),
    ])
}

fn clip_json(c: &ClipDef) -> Json {
    obj(vec![
        ("pattern", c.pattern.into()),
        ("lane", c.lane.into()),
        ("start", c.start.into()),
        ("len", c.len.into()),
        ("offset", c.offset.into()),
    ])
}

fn track_json(t: &TrackDef) -> Json {
    let sends = t.sends.iter().map(|&(target, amount)| Json::Arr(vec![target.into(), amount.into()])).collect();
    let inserts = t
        .inserts
        .iter()
        .map(|fx| match fx {
            None => Json::Null,
            Some(fx) => obj(vec![
                ("kind", fx.kind.as_str().into()),
                ("params", params_json(&fx.params)),
                ("bypass", fx.bypass.into()),
            ]),
        })
        .collect();
    obj(vec![
        ("name", t.name.as_str().into()),
        ("gain", t.gain.into()),
        ("pan", t.pan.into()),
        ("muted", t.muted.into()),
        ("output", t.output.into()),
        ("color", t.color.into()),
        ("sends", Json::Arr(sends)),
        ("inserts", Json::Arr(inserts)),
    ])
}

/// Projet → texte JSON indenté.
pub fn save(project: &Project) -> String {
    let mode = match project.mode {
        PlayMode::Pattern(n) => obj(vec![("pattern", n.into())]),
        PlayMode::Song => "song".into(),
    };
    obj(vec![
        ("format", FORMAT.into()),
        ("version", VERSION.into()),
        ("name", project.name.as_str().into()),
        ("bpm", project.bpm.into()),
        ("swing", project.swing.into()),
        ("mode", mode),
        ("current_pattern", project.current_pattern.into()),
        ("channels", Json::Arr(project.channels.iter().map(channel_json).collect())),
        ("patterns", Json::Arr(project.patterns.iter().map(pattern_json).collect())),
        ("clips", Json::Arr(project.clips.iter().map(clip_json).collect())),
        ("tracks", Json::Arr(project.tracks.iter().map(track_json).collect())),
    ])
    .pretty()
}

/// Écrit le projet dans un fichier (via un fichier temporaire, pour ne jamais
/// laisser de sauvegarde à moitié écrite).
pub fn save_to_file(project: &Project, path: impl AsRef<Path>) -> Result<(), String> {
    let path = path.as_ref();
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);
    let fail = |e: std::io::Error| format!("impossible d'écrire « {} » : {e}", path.display());
    std::fs::write(&tmp, save(project)).map_err(fail)?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        fail(e)
    })
}

// --- Chargement ---

fn num(v: Option<&Json>) -> Option<f64> {
    v.and_then(Json::as_f64).filter(|n| n.is_finite())
}

fn f32_or(v: Option<&Json>, default: f32) -> f32 {
    num(v).map_or(default, |n| n as f32).clamp(f32::MIN, f32::MAX)
}

fn u32_or(v: Option<&Json>, default: u32) -> u32 {
    num(v).map_or(default, |n| n.round().clamp(0.0, u32::MAX as f64) as u32)
}

fn usize_or(v: Option<&Json>, default: usize) -> usize {
    num(v).map_or(default, |n| n.round().clamp(0.0, u32::MAX as f64) as usize)
}

fn bool_or(v: Option<&Json>, default: bool) -> bool {
    v.and_then(Json::as_bool).unwrap_or(default)
}

fn string_or(v: Option<&Json>, default: &str) -> String {
    v.and_then(Json::as_str).unwrap_or(default).to_string()
}

fn arr(v: Option<&Json>) -> &[Json] {
    v.and_then(Json::as_arr).unwrap_or(&[])
}

fn gain(v: Option<&Json>, default: f32) -> f32 {
    f32_or(v, default).clamp(0.0, 16.0)
}

fn pan(v: Option<&Json>) -> f32 {
    f32_or(v, 0.0).clamp(-1.0, 1.0)
}

/// `[[id, valeur], …]` ; les entrées malformées sont ignorées, un identifiant
/// répété garde sa dernière valeur.
fn load_params(v: Option<&Json>) -> Vec<(u32, f32)> {
    let mut out: Vec<(u32, f32)> = Vec::new();
    for p in arr(v) {
        let p = arr(Some(p));
        let (Some(id), Some(value)) = (num(p.first()), num(p.get(1))) else { continue };
        if id < 0.0 || id > u32::MAX as f64 {
            continue;
        }
        let id = id as u32;
        let value = (value as f32).clamp(f32::MIN, f32::MAX);
        match out.iter_mut().find(|(i, _)| *i == id) {
            Some(p) => p.1 = value,
            None => out.push((id, value)),
        }
    }
    out
}

fn load_channel(v: &Json) -> ChannelDef {
    let inst = v.get("instrument");
    let field = |k| inst.and_then(|i| i.get(k));
    ChannelDef {
        name: string_or(v.get("name"), "Canal"),
        instrument: InstrumentDef {
            kind: string_or(field("kind"), "pulse"),
            params: load_params(field("params")),
            sample: field("sample").and_then(Json::as_str).map(String::from),
        },
        gain: gain(v.get("gain"), 0.8),
        pan: pan(v.get("pan")),
        muted: bool_or(v.get("muted"), false),
        track: usize_or(v.get("track"), 1).min(TRACKS - 1),
        root_note: u32_or(v.get("root_note"), 60).min(127) as u8,
        color: u32_or(v.get("color"), 0),
    }
}

fn load_note(v: &Json) -> Option<Note> {
    let n = v.as_arr()?;
    Some(Note {
        start: u32_or(Some(n.first()?), 0),
        len: u32_or(n.get(1), 1).max(1),
        key: u32_or(n.get(2), 60).min(127) as u8,
        vel: f32_or(n.get(3), 0.8).clamp(0.0, 1.0),
    })
}

fn load_pattern(v: &Json, index: usize, channels: usize) -> PatternDef {
    let mut notes: Vec<Vec<Note>> = arr(v.get("notes"))
        .iter()
        .take(channels)
        .map(|ch| {
            let mut notes: Vec<Note> = arr(Some(ch)).iter().filter_map(load_note).collect();
            notes.sort_by_key(|n| (n.start, n.key));
            notes
        })
        .collect();
    notes.resize(channels, Vec::new());
    PatternDef {
        name: string_or(v.get("name"), &format!("Pattern {}", index + 1)),
        len: u32_or(v.get("len"), TICKS_PER_BAR).max(1),
        notes,
        color: u32_or(v.get("color"), 0),
    }
}

fn load_clip(v: &Json, patterns: usize) -> Option<ClipDef> {
    let pattern = num(v.get("pattern"))?;
    if pattern < 0.0 || pattern.fract() != 0.0 || pattern >= patterns as f64 {
        return None;
    }
    Some(ClipDef {
        pattern: pattern as usize,
        lane: usize_or(v.get("lane"), 0),
        start: u32_or(v.get("start"), 0),
        len: u32_or(v.get("len"), TICKS_PER_BAR).max(1),
        offset: u32_or(v.get("offset"), 0),
    })
}

fn load_effect(v: &Json) -> Option<EffectDef> {
    Some(EffectDef {
        kind: v.get("kind")?.as_str()?.to_string(),
        params: load_params(v.get("params")),
        bypass: bool_or(v.get("bypass"), false),
    })
}

fn load_track(v: &Json, index: usize) -> TrackDef {
    let mut t = TrackDef::new(if index == MASTER { "Master".to_string() } else { format!("Piste {index}") });
    t.name = string_or(v.get("name"), &t.name);
    t.gain = gain(v.get("gain"), 1.0);
    t.pan = pan(v.get("pan"));
    t.muted = bool_or(v.get("muted"), false);
    t.output = if index == MASTER { MASTER } else { usize_or(v.get("output"), MASTER) };
    if t.output >= TRACKS || t.output == index {
        t.output = MASTER;
    }
    t.color = u32_or(v.get("color"), 0);
    for (slot, s) in arr(v.get("sends")).iter().take(SEND_SLOTS).enumerate() {
        let s = arr(Some(s));
        let target = usize_or(s.first(), MASTER);
        let amount = f32_or(s.get(1), 0.0).clamp(0.0, 16.0);
        t.sends[slot] = if target < TRACKS && target != index { (target, amount) } else { (MASTER, 0.0) };
    }
    for (slot, fx) in arr(v.get("inserts")).iter().take(INSERT_SLOTS).enumerate() {
        t.inserts[slot] = load_effect(fx);
    }
    t
}

/// Texte JSON → projet.
pub fn load(text: &str) -> Result<Project, String> {
    let root = json::parse(text).map_err(|e| format!("fichier de projet illisible : {e}"))?;
    if root.as_obj().is_none() {
        return Err("ce fichier n'est pas un projet Ponzik".into());
    }
    match root.get("format").and_then(Json::as_str) {
        Some(FORMAT) => {}
        Some(other) => return Err(format!("format « {other} » inconnu : ce fichier n'est pas un projet Ponzik")),
        None => return Err("ce fichier n'est pas un projet Ponzik (champ « format » absent)".into()),
    }
    if let Some(v) = num(root.get("version"))
        && v.floor() > VERSION as f64
    {
        return Err(format!(
            "ce projet utilise la version {} du format, plus récente que celle de cette version de Ponzik ({VERSION}) : \
             mettez Ponzik à jour",
            json::format_number(v.floor())
        ));
    }

    let mut p = Project::empty();
    p.name = string_or(root.get("name"), &p.name);
    p.bpm = f32_or(root.get("bpm"), p.bpm).clamp(20.0, 999.0);
    p.swing = f32_or(root.get("swing"), 0.0).clamp(0.0, 1.0);

    p.channels = arr(root.get("channels")).iter().map(load_channel).collect();
    let n = p.channels.len();
    let patterns: Vec<PatternDef> =
        arr(root.get("patterns")).iter().enumerate().map(|(i, v)| load_pattern(v, i, n)).collect();
    if !patterns.is_empty() {
        p.patterns = patterns;
    } else {
        p.patterns[0].notes.resize(n, Vec::new());
    }
    let np = p.patterns.len();
    p.clips = arr(root.get("clips")).iter().filter_map(|c| load_clip(c, np)).collect();

    let tracks = arr(root.get("tracks"));
    for (i, t) in p.tracks.iter_mut().enumerate() {
        if let Some(v) = tracks.get(i) {
            *t = load_track(v, i);
        }
    }
    if p.routing_order().is_none() {
        // Routage en boucle : on renvoie tout au master plutôt que de refuser le fichier.
        for t in &mut p.tracks {
            t.output = MASTER;
            t.sends = [(MASTER, 0.0); SEND_SLOTS];
        }
    }

    p.mode = match root.get("mode") {
        Some(Json::Str(s)) if s == "song" => PlayMode::Song,
        Some(m) => PlayMode::Pattern(usize_or(m.get("pattern"), 0).min(np - 1)),
        None => PlayMode::Pattern(0),
    };
    p.current_pattern = usize_or(root.get("current_pattern"), 0).min(np - 1);
    Ok(p)
}

pub fn load_from_file(path: impl AsRef<Path>) -> Result<Project, String> {
    let path = path.as_ref();
    let bytes = std::fs::read(path).map_err(|e| format!("impossible de lire « {} » : {e}", path.display()))?;
    let text = String::from_utf8(bytes)
        .map_err(|_| format!("« {} » n'est pas un fichier texte UTF-8 valide", path.display()))?;
    load(&text).map_err(|e| format!("« {} » : {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demo_project;

    #[test]
    fn demo_round_trips() {
        let mut p = demo_project();
        // Quelques valeurs non triviales pour tout couvrir.
        p.swing = 0.37;
        p.mode = PlayMode::Song;
        p.current_pattern = 2;
        p.channels[0].instrument.sample = Some("kits/kick \"808\".wav".into());
        p.channels[1].color = 0xFF_80_40;
        p.tracks[3].output = 2;
        p.tracks[3].sends[1] = (5, 0.25);
        let mut fx = EffectDef::new("reverb");
        fx.set(0, 0.123_456_79);
        fx.bypass = true;
        p.tracks[2].inserts[4] = Some(fx);
        let text = save(&p);
        assert!(text.starts_with("{\n  \"format\": \"ponzik-project\",\n  \"version\": 1,"));
        let back = load(&text).unwrap();
        assert_eq!(back, p);
        assert_eq!(save(&back), text);

        let plain = demo_project();
        assert_eq!(load(&save(&plain)).unwrap(), plain);
    }

    #[test]
    fn file_round_trip() {
        let dir = std::env::temp_dir().join(format!("ponzik-io-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("demo.ponzik");
        let p = demo_project();
        save_to_file(&p, &path).unwrap();
        assert_eq!(load_from_file(&path).unwrap(), p);
        assert!(load_from_file(dir.join("absent.ponzik")).unwrap_err().contains("impossible de lire"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn minimal_file_gets_defaults() {
        let p = load(r#"{"format": "ponzik-project"}"#).unwrap();
        assert_eq!(p, Project::empty());
    }

    #[test]
    fn tolerant_loading() {
        let text = r#"{
          "format": "ponzik-project", "version": 1.4, "futur": {"x": [1, 2]},
          "bpm": 5000, "swing": "beaucoup",
          "channels": [{"name": "A", "track": 99, "root_note": 300, "pan": -7}, {}],
          "patterns": [
            {"name": "P", "len": 0, "notes": [[[96, 24, 60, 2], [0, 0, 62], "x", [48]], [], [[0, 1, 1, 1]]]},
            {}
          ],
          "clips": [
            {"pattern": 1, "start": 384}, {"pattern": 2}, {"pattern": -1}, {"pattern": 0.5}, {"start": 3}
          ],
          "tracks": [{"name": "Principal", "output": 4}, {"output": 2, "sends": [[1, 0.5], [77, 1]]},
                     {"output": 1}, {"inserts": [null, {"kind": "eq"}, {"params": []}]}],
          "mode": {"pattern": 42},
          "current_pattern": -3
        }"#;
        let p = load(text).unwrap();
        assert_eq!(p.bpm, 999.0);
        assert_eq!(p.swing, 0.0);
        assert_eq!(p.channels.len(), 2);
        assert_eq!(p.channels[0].track, TRACKS - 1);
        assert_eq!(p.channels[0].root_note, 127);
        assert_eq!(p.channels[0].pan, -1.0);
        assert_eq!(p.channels[1].name, "Canal");
        assert_eq!(p.channels[1].instrument.kind, "pulse");
        assert_eq!(p.patterns.len(), 2);
        let pat = &p.patterns[0];
        assert_eq!(pat.len, 1);
        assert_eq!(pat.notes.len(), 2, "canaux en trop retirés");
        let notes = &pat.notes[0];
        assert_eq!(notes.iter().map(|n| n.start).collect::<Vec<_>>(), [0, 48, 96], "notes triées");
        assert_eq!(notes[0].len, 1);
        assert_eq!(notes[0].vel, 0.8);
        assert_eq!(notes[2].vel, 1.0);
        assert_eq!(p.patterns[1].name, "Pattern 2");
        assert_eq!(p.patterns[1].notes.len(), 2);
        assert_eq!(p.clips.len(), 1);
        assert_eq!(p.clips[0].pattern, 1);
        assert_eq!(p.tracks.len(), TRACKS);
        assert_eq!(p.tracks[MASTER].name, "Principal");
        assert_eq!(p.tracks[MASTER].output, MASTER);
        assert_eq!(p.tracks[5].name, "Piste 5");
        // Les pistes 1 et 2 bouclaient : tout est renvoyé au master.
        assert!(p.tracks.iter().all(|t| t.output == MASTER));
        assert!(p.routing_order().is_some());
        assert_eq!(p.tracks[3].inserts[1].as_ref().map(|f| f.kind.as_str()), Some("eq"));
        assert!(p.tracks[3].inserts[2].is_none());
        assert_eq!(p.mode, PlayMode::Pattern(1));
        assert_eq!(p.current_pattern, 0);
        // Un projet chargé se construit et se sauvegarde sans erreur.
        assert_eq!(load(&save(&p)).unwrap(), p);
    }

    #[test]
    fn too_many_tracks_are_truncated() {
        let tracks = vec!["{}"; TRACKS + 5].join(",");
        let p = load(&format!(r#"{{"format": "ponzik-project", "tracks": [{tracks}]}}"#)).unwrap();
        assert_eq!(p.tracks.len(), TRACKS);
    }

    #[test]
    fn rejects_foreign_and_future_files() {
        let e = load(r#"{"format": "autre-daw", "version": 1}"#).unwrap_err();
        assert!(e.contains("autre-daw") && e.contains("pas un projet Ponzik"), "{e}");
        assert!(load(r#"{"version": 1}"#).unwrap_err().contains("format"));
        assert!(load("[1, 2]").unwrap_err().contains("pas un projet Ponzik"));
        let e = load(r#"{"format": "ponzik-project", "version": 2}"#).unwrap_err();
        assert!(e.contains("version 2") && e.contains("mettez Ponzik à jour"), "{e}");
        let e = load("{\n  \"format\": \"ponzik-project\",\n  \"bpm\": 12O\n}").unwrap_err();
        assert!(e.contains("illisible") && e.contains("ligne 3"), "{e}");
    }
}
