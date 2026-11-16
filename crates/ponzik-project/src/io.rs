//! Fichiers de projet `.ponzik` : JSON lisible, chargement tolérant.
//!
//! Les champs absents prennent leur valeur par défaut, les champs inconnus sont
//! ignorés et tous les index sont vérifiés : un fichier abîmé ou écrit par une
//! version plus récente (même majeure) se charge au mieux.

use std::path::Path;

use ponzik_i18n::{tr, tr_fmt};

use ponzik_engine::mixer::{INSERT_SLOTS, MASTER, SEND_SLOTS, TRACKS};
use ponzik_engine::instruments::drum_pads::{LAYERS, PADS as PAD_LIMIT};
use ponzik_engine::{NOTE_FX_SLOTS, Note, PlayMode, TICKS_PER_BAR};

use crate::json::{self, Json};
use crate::{
    AudioClipDef, AutoPoint, AutoScope, AutoTarget, AutomationDef, ChannelDef, ClipDef, EffectDef, InstrumentDef,
    NoteFxDef, PatternDef, Project, TrackDef, TrackKind,
};

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

fn instrument_json(i: &InstrumentDef) -> Json {
    let mut pairs = vec![
        ("kind", i.kind.as_str().into()),
        ("params", params_json(&i.params)),
        ("sample", i.sample.clone().into()),
    ];
    // Optional field: only pad instruments with at least one sample write it.
    let used = i.pads.iter().rposition(|p| !p.is_empty()).map_or(0, |n| n + 1);
    if used > 0 {
        let pads = i.pads[..used]
            .iter()
            .map(|layers| Json::Arr(layers.iter().map(|p| p.as_str().into()).collect()))
            .collect();
        pairs.push(("pads", Json::Arr(pads)));
    }
    obj(pairs)
}

fn channel_json(c: &ChannelDef) -> Json {
    let mut pairs = vec![
        ("name", c.name.as_str().into()),
        ("instrument", instrument_json(&c.instrument)),
        ("gain", c.gain.into()),
        ("pan", c.pan.into()),
        ("muted", c.muted.into()),
        ("track", c.track.into()),
        ("root_note", c.root_note.into()),
        ("color", c.color.into()),
    ];
    // Champ facultatif : absent tant que le canal n'a pas d'effet de notes.
    if !c.note_fx.is_empty() {
        let slots = c
            .note_fx
            .iter()
            .map(|fx| match fx {
                None => Json::Null,
                Some(fx) => obj(vec![("kind", fx.kind.as_str().into()), ("params", params_json(&fx.params))]),
            })
            .collect();
        pairs.push(("note_fx", Json::Arr(slots)));
    }
    obj(pairs)
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
            Some(fx) => {
                let mut pairs = vec![
                    ("kind", fx.kind.as_str().into()),
                    ("params", params_json(&fx.params)),
                    ("bypass", fx.bypass.into()),
                ];
                if let Some(key) = fx.sidechain {
                    pairs.push(("sidechain", key.into()));
                }
                obj(pairs)
            }
        })
        .collect();
    let mut pairs = vec![
        ("name", t.name.as_str().into()),
        ("gain", t.gain.into()),
        ("pan", t.pan.into()),
        ("muted", t.muted.into()),
        ("output", t.output.into()),
        ("color", t.color.into()),
        ("sends", Json::Arr(sends)),
        ("inserts", Json::Arr(inserts)),
    ];
    // Champs ajoutés après la version 1 : écrits seulement s'ils servent.
    if t.kind != TrackKind::Normal {
        pairs.push(("kind", t.kind.id().into()));
    }
    if t.folded {
        pairs.push(("folded", true.into()));
    }
    if let Some(vca) = t.vca {
        pairs.push(("vca", vca.into()));
    }
    obj(pairs)
}

fn target_json(t: AutoTarget) -> Json {
    let kind = |k: &str, rest: Vec<(&str, Json)>| {
        let mut pairs = vec![("kind", k.into())];
        pairs.extend(rest);
        obj(pairs)
    };
    match t {
        AutoTarget::InstrumentParam { channel, id } => {
            kind("instrument_param", vec![("channel", channel.into()), ("id", id.into())])
        }
        AutoTarget::EffectParam { track, slot, id } => {
            kind("effect_param", vec![("track", track.into()), ("slot", slot.into()), ("id", id.into())])
        }
        AutoTarget::ChannelGain(c) => kind("channel_gain", vec![("channel", c.into())]),
        AutoTarget::ChannelPan(c) => kind("channel_pan", vec![("channel", c.into())]),
        AutoTarget::TrackGain(t) => kind("track_gain", vec![("track", t.into())]),
        AutoTarget::TrackPan(t) => kind("track_pan", vec![("track", t.into())]),
        AutoTarget::Tempo => kind("tempo", Vec::new()),
    }
}

fn automation_json(a: &AutomationDef) -> Json {
    let scope = match a.scope {
        AutoScope::Song => "song".into(),
        AutoScope::Pattern(n) => obj(vec![("pattern", n.into())]),
    };
    let points = a.points.iter().map(|p| Json::Arr(vec![p.tick.into(), p.value.into(), p.curve.into()])).collect();
    obj(vec![("target", target_json(a.target)), ("scope", scope), ("points", Json::Arr(points))])
}

fn audio_clip_json(c: &AudioClipDef) -> Json {
    obj(vec![
        ("path", c.path.as_str().into()),
        ("lane", c.lane.into()),
        ("track", c.track.into()),
        ("start", c.start.into()),
        ("len", c.len.into()),
        ("offset", c.offset.into()),
        ("gain", c.gain.into()),
        ("fade_in", c.fade_in.into()),
        ("fade_out", c.fade_out.into()),
        ("take", c.take.into()),
        ("muted", c.muted.into()),
    ])
}

/// Projet → texte JSON indenté.
pub fn save(project: &Project) -> String {
    let mode = match project.mode {
        PlayMode::Pattern(n) => obj(vec![("pattern", n.into())]),
        PlayMode::Song => "song".into(),
        PlayMode::Session => "session".into(),
    };
    obj(vec![
        ("format", FORMAT.into()),
        ("version", VERSION.into()),
        ("name", project.name.as_str().into()),
        ("bpm", project.bpm.into()),
        ("swing", project.swing.into()),
        ("key", project.key.into()),
        ("scale", project.scale.id().into()),
        ("mode", mode),
        ("current_pattern", project.current_pattern.into()),
        ("channels", Json::Arr(project.channels.iter().map(channel_json).collect())),
        ("patterns", Json::Arr(project.patterns.iter().map(pattern_json).collect())),
        ("clips", Json::Arr(project.clips.iter().map(clip_json).collect())),
        ("tracks", Json::Arr(project.tracks.iter().map(track_json).collect())),
        ("automation", Json::Arr(project.automation.iter().map(automation_json).collect())),
        ("audio_clips", Json::Arr(project.audio_clips.iter().map(audio_clip_json).collect())),
        (
            "tempo_changes",
            Json::Arr(
                project
                    .tempo_changes
                    .iter()
                    .map(|p| obj(vec![("tick", p.tick.into()), ("bpm", p.bpm.into()), ("ramp", p.ramp.into())]))
                    .collect(),
            ),
        ),
        (
            "signatures",
            Json::Arr(
                project
                    .signatures
                    .iter()
                    .map(|s| obj(vec![("tick", s.tick.into()), ("num", (s.num as u32).into()), ("den", (s.den as u32).into())]))
                    .collect(),
            ),
        ),
        (
            "markers",
            Json::Arr(
                project
                    .markers
                    .iter()
                    .map(|m| obj(vec![("tick", m.tick.into()), ("name", m.name.as_str().into()), ("color", m.color.into())]))
                    .collect(),
            ),
        ),
        (
            "session",
            Json::Arr(
                project
                    .session_grid
                    .iter()
                    .map(|col| Json::Arr(col.iter().map(|c| c.map_or(Json::Null, |p| p.into())).collect()))
                    .collect(),
            ),
        ),
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
    let fail = |e: std::io::Error| tr_fmt!("Can't write \"{}\": {}", path.display().to_string(), e.to_string());
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

/// Emplacements d'effets de notes (`null` ou malformé : vide), au plus [`NOTE_FX_SLOTS`].
fn load_note_fx(v: Option<&Json>) -> Vec<Option<NoteFxDef>> {
    arr(v)
        .iter()
        .take(NOTE_FX_SLOTS)
        .map(|fx| {
            let kind = fx.get("kind")?.as_str()?.to_string();
            Some(NoteFxDef { kind, params: load_params(fx.get("params")) })
        })
        .collect()
}

/// Pad sample files: at most [`PAD_LIMIT`] pads of [`LAYERS`] layers; anything
/// that is not a string is skipped.
fn load_pads(v: Option<&Json>) -> Vec<Vec<String>> {
    arr(v)
        .iter()
        .take(PAD_LIMIT)
        .map(|layers| arr(Some(layers)).iter().filter_map(Json::as_str).take(LAYERS).map(String::from).collect())
        .collect()
}

fn load_channel(v: &Json) -> ChannelDef {
    let inst = v.get("instrument");
    let field = |k| inst.and_then(|i| i.get(k));
    ChannelDef {
        name: string_or(v.get("name"), tr("Channel")),
        instrument: InstrumentDef {
            kind: string_or(field("kind"), "pulse"),
            params: load_params(field("params")),
            sample: field("sample").and_then(Json::as_str).map(String::from),
            pads: load_pads(field("pads")),
        },
        note_fx: load_note_fx(v.get("note_fx")),
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
        name: string_or(v.get("name"), &tr_fmt!("Pattern {}", index + 1)),
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
        sidechain: index(v.get("sidechain"), TRACKS),
    })
}

fn load_track(v: &Json, index: usize) -> TrackDef {
    let mut t = TrackDef::new(if index == MASTER { tr("Master").to_string() } else { crate::default_track_name(index) });
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
    if index != MASTER {
        t.kind = match v.get("kind") {
            Some(Json::Str(k)) => TrackKind::from_id(k).unwrap_or_default(),
            _ => TrackKind::Normal,
        };
        t.folded = t.kind == TrackKind::Bus && bool_or(v.get("folded"), false);
        t.vca = crate::io::index(v.get("vca"), TRACKS).filter(|&x| x != index && x != MASTER);
    }
    for (slot, fx) in arr(v.get("inserts")).iter().take(INSERT_SLOTS).enumerate() {
        t.inserts[slot] = load_effect(fx);
        if let Some(fx) = &mut t.inserts[slot]
            && fx.sidechain == Some(index)
        {
            fx.sidechain = None;
        }
    }
    t
}

/// Indice entier dans [0, `limit`).
fn index(v: Option<&Json>, limit: usize) -> Option<usize> {
    let n = num(v)?;
    (n >= 0.0 && n.fract() == 0.0 && n < limit as f64).then_some(n as usize)
}

fn load_target(v: &Json, channels: usize) -> Option<AutoTarget> {
    let channel = || index(v.get("channel"), channels);
    let track = || index(v.get("track"), TRACKS);
    let id = || num(v.get("id")).filter(|n| *n >= 0.0 && n.fract() == 0.0 && *n <= u32::MAX as f64).map(|n| n as u32);
    Some(match v.get("kind")?.as_str()? {
        "instrument_param" => AutoTarget::InstrumentParam { channel: channel()?, id: id()? },
        "effect_param" => {
            AutoTarget::EffectParam { track: track()?, slot: index(v.get("slot"), INSERT_SLOTS)?, id: id()? }
        }
        "channel_gain" => AutoTarget::ChannelGain(channel()?),
        "channel_pan" => AutoTarget::ChannelPan(channel()?),
        "track_gain" => AutoTarget::TrackGain(track()?),
        "track_pan" => AutoTarget::TrackPan(track()?),
        "tempo" => AutoTarget::Tempo,
        _ => return None,
    })
}

/// Courbe valide (cible et pattern existants, au moins un point), points triés,
/// un seul par tick (le dernier lu gagne).
fn load_automation(v: &Json, channels: usize, patterns: usize) -> Option<AutomationDef> {
    let target = load_target(v.get("target")?, channels)?;
    let scope = match v.get("scope") {
        None => AutoScope::Song,
        Some(Json::Str(s)) if s == "song" => AutoScope::Song,
        Some(s) => AutoScope::Pattern(index(s.get("pattern"), patterns)?),
    };
    let mut points: Vec<AutoPoint> = Vec::new();
    for p in arr(v.get("points")) {
        let p = arr(Some(p));
        let Some(value) = num(p.get(1)) else { continue };
        let point = AutoPoint {
            tick: u32_or(p.first(), 0),
            value: (value as f32).clamp(f32::MIN, f32::MAX),
            curve: f32_or(p.get(2), 0.0).clamp(-1.0, 1.0),
        };
        match points.iter_mut().find(|q| q.tick == point.tick) {
            Some(q) => *q = point,
            None => points.push(point),
        }
    }
    points.sort_by_key(|p| p.tick);
    (!points.is_empty()).then_some(AutomationDef { target, scope, points })
}

fn load_audio_clip(v: &Json) -> Option<AudioClipDef> {
    let path = v.get("path")?.as_str().filter(|p| !p.is_empty())?.to_string();
    Some(AudioClipDef {
        path,
        lane: usize_or(v.get("lane"), 0),
        track: usize_or(v.get("track"), 1).min(TRACKS - 1),
        start: u32_or(v.get("start"), 0),
        len: u32_or(v.get("len"), TICKS_PER_BAR).max(1),
        offset: num(v.get("offset")).unwrap_or(0.0).max(0.0),
        gain: gain(v.get("gain"), 1.0),
        fade_in: f32_or(v.get("fade_in"), 0.0).max(0.0),
        fade_out: f32_or(v.get("fade_out"), 0.0).max(0.0),
        take: u32_or(v.get("take"), 0),
        muted: v.get("muted").and_then(Json::as_bool).unwrap_or(false),
    })
}

/// Texte JSON → projet.
pub fn load(text: &str) -> Result<Project, String> {
    let root = json::parse(text).map_err(|e| tr_fmt!("Unreadable project file: {}", e))?;
    if root.as_obj().is_none() {
        return Err(tr("This file is not a Ponzik project").into());
    }
    match root.get("format").and_then(Json::as_str) {
        Some(FORMAT) => {}
        Some(other) => return Err(tr_fmt!("Unknown format \"{}\": this file is not a Ponzik project", other)),
        None => return Err(tr("This file is not a Ponzik project (no \"format\" field)").into()),
    }
    if let Some(v) = num(root.get("version"))
        && v.floor() > VERSION as f64
    {
        return Err(tr_fmt!(
            "This project uses version {} of the format, newer than this version of Ponzik supports ({}): please update Ponzik",
            json::format_number(v.floor()),
            VERSION
        ));
    }

    let mut p = Project::empty();
    p.name = string_or(root.get("name"), &p.name);
    p.bpm = f32_or(root.get("bpm"), p.bpm).clamp(20.0, 999.0);
    p.swing = f32_or(root.get("swing"), 0.0).clamp(0.0, 1.0);
    // Gamme : champs facultatifs, une valeur inconnue revient à la gamme chromatique.
    p.key = (u32_or(root.get("key"), 0) % 12) as u8;
    p.scale = root.get("scale").and_then(Json::as_str).and_then(crate::ScaleKind::from_id).unwrap_or_default();

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
    // Un VCA ne suit que les VCA : une piste pilotée par une piste qui n'en est pas un est libérée.
    let kinds: Vec<TrackKind> = p.tracks.iter().map(|t| t.kind).collect();
    for t in &mut p.tracks {
        if t.kind == TrackKind::Vca || t.vca.is_some_and(|v| kinds[v] != TrackKind::Vca) {
            t.vca = None;
        }
    }
    if p.routing_order().is_none() {
        // Routage en boucle : on renvoie tout au master et on débranche les clés
        // plutôt que de refuser le fichier.
        for t in &mut p.tracks {
            t.output = MASTER;
            t.sends = [(MASTER, 0.0); SEND_SLOTS];
            for fx in t.inserts.iter_mut().flatten() {
                fx.sidechain = None;
            }
        }
    }

    p.mode = match root.get("mode") {
        Some(Json::Str(s)) if s == "song" => PlayMode::Song,
        Some(Json::Str(s)) if s == "session" => PlayMode::Session,
        Some(m) => PlayMode::Pattern(usize_or(m.get("pattern"), 0).min(np - 1)),
        None => PlayMode::Pattern(0),
    };
    p.current_pattern = usize_or(root.get("current_pattern"), 0).min(np - 1);
    // Grille de session : dimensions fixes, patterns hors limites oubliés.
    for (c, col) in arr(root.get("session")).iter().take(crate::SESSION_COLUMNS).enumerate() {
        for (r, cell) in arr(Some(col)).iter().take(crate::SESSION_SCENES).enumerate() {
            p.session_grid[c][r] = cell.as_f64().map(|v| v as usize).filter(|&v| v < np);
        }
    }

    for a in arr(root.get("automation")).iter().filter_map(|a| load_automation(a, n, np)) {
        // Une seule courbe par cible et portée : la première gagne.
        if p.automation_lane(a.target, a.scope).is_none() {
            p.automation.push(a);
        }
    }
    p.audio_clips = arr(root.get("audio_clips")).iter().filter_map(load_audio_clip).collect();
    // Tempo, meter and markers: optional (older projects: one tempo, 4/4, no markers).
    for v in arr(root.get("tempo_changes")) {
        if let Some(bpm) = num(v.get("bpm")).filter(|_| v.get("tick").is_some()) {
            let tick = u32_or(v.get("tick"), 0);
            if tick > 0 {
                p.set_tempo_change(tick, bpm as f32, bool_or(v.get("ramp"), false));
            }
        }
    }
    for v in arr(root.get("signatures")) {
        let (num, den) = (u32_or(v.get("num"), 0), u32_or(v.get("den"), 0));
        if (1..=32).contains(&num) && den <= 32 && crate::TimeSig::valid_den(den as u8) {
            p.set_signature(u32_or(v.get("tick"), 0), num as u8, den as u8);
        }
    }
    p.normalize_tempo();
    for v in arr(root.get("markers")) {
        let Some(tick) = num(v.get("tick")).map(|t| t.round().clamp(0.0, u32::MAX as f64) as u32) else { continue };
        let i = p.add_marker(tick, string_or(v.get("name"), ""));
        p.markers[i].color = u32_or(v.get("color"), p.markers[i].color) & 0xff_ffff;
    }
    Ok(p)
}

pub fn load_from_file(path: impl AsRef<Path>) -> Result<Project, String> {
    let path = path.as_ref();
    let bytes = std::fs::read(path).map_err(|e| tr_fmt!("Can't read \"{}\": {}", path.display().to_string(), e.to_string()))?;
    let text = String::from_utf8(bytes)
        .map_err(|_| tr_fmt!("\"{}\" is not a valid UTF-8 text file", path.display().to_string()))?;
    load(&text).map_err(|e| tr_fmt!("\"{}\": {}", path.display().to_string(), e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tempo_meter_and_markers_roundtrip_and_old_files_load() {
        let mut p = crate::demo_project();
        p.set_tempo_change(768, 140.0, true);
        p.set_tempo_change(1536, 90.5, false);
        p.set_signature(1536, 7, 8);
        let i = p.add_marker(384, "Chorus");
        p.markers[i].color = 0xe878b4;
        let back = load(&save(&p)).unwrap();
        assert_eq!(back.tempo_changes, p.tempo_changes);
        assert_eq!(back.signatures, p.signatures);
        assert_eq!(back.markers, p.markers);
        // Without the fields: one tempo, 4/4, no markers.
        let old = load(r#"{"format": "ponzik-project", "bpm": 128}"#).unwrap();
        assert_eq!(old.tempo_map(), ponzik_engine::TempoMap::constant(128.0));
        assert!(old.markers.is_empty());
        // Broken entries are skipped.
        let text = r#"{"format": "ponzik-project", "tempo_changes": [{"tick": 96}, {"tick": 0, "bpm": 10}, {"tick": 384, "bpm": 1e9}],
            "signatures": [{"tick": 0, "num": 0, "den": 4}, {"tick": 384, "num": 3, "den": 5}, {"tick": 384, "num": 3, "den": 4}],
            "markers": [{"name": "x"}, {"tick": 768, "name": "Drop"}]}"#;
        let q = load(text).unwrap();
        assert_eq!(q.tempo_changes.len(), 1);
        assert_eq!(q.tempo_changes[0].bpm, 999.0);
        assert_eq!(q.signatures, [crate::TimeSig { tick: 384, num: 3, den: 4 }]);
        assert_eq!(q.markers.len(), 1);
        assert_eq!(q.markers[0].tick, 768);
    }

    #[test]
    fn session_grid_roundtrip() {
        let mut p = crate::demo_project();
        p.mode = PlayMode::Session;
        p.session_grid[0][0] = Some(1);
        p.session_grid[3][7] = Some(2);
        let back = load(&save(&p)).unwrap();
        assert_eq!(back.mode, PlayMode::Session);
        assert_eq!(back.session_grid, p.session_grid);
    }
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
    fn drum_pads_round_trip() {
        let mut p = Project::empty();
        let mut def = InstrumentDef::new("drum_pads");
        def.set(ponzik_engine::instruments::drum_pads::pad_param(6, 11), 2.0);
        def.pads = vec![vec!["kick.wav".into()], Vec::new(), vec!["snare soft.wav".into(), "snare \"hard\".wav".into()]];
        p.add_channel("Pads", def, 36);
        let text = save(&p);
        assert!(text.contains("\"pads\""));
        let back = load(&text).unwrap();
        assert_eq!(back, p);
        assert_eq!(save(&back), text);
        // Trailing empty pads are not written, and come back empty.
        p.channels[0].instrument.pads.push(Vec::new());
        assert_eq!(save(&p), text);
        // No pads field for other instruments.
        assert!(!save(&demo_project()).contains("\"pads\""));
    }

    #[test]
    fn malformed_pads_are_tolerated() {
        let text = r#"{"format": "ponzik-project", "channels": [{"instrument": {"kind": "drum_pads",
            "pads": [["a.wav", 3, null, "b.wav", "c.wav", "d.wav", "e.wav"], "oops", {"x": 1}]}}]}"#;
        let p = load(text).unwrap();
        let pads = &p.channels[0].instrument.pads;
        assert_eq!(pads[0], ["a.wav", "b.wav", "c.wav", "d.wav"]);
        assert!(pads[1].is_empty() && pads[2].is_empty());
        let text = r#"{"format": "ponzik-project", "channels": [{"instrument": {"kind": "drum_pads", "pads": 7}}]}"#;
        assert!(load(text).unwrap().channels[0].instrument.pads.is_empty());
    }

    #[test]
    fn file_round_trip() {
        let dir = std::env::temp_dir().join(format!("ponzik-io-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("demo.ponzik");
        let p = demo_project();
        save_to_file(&p, &path).unwrap();
        assert_eq!(load_from_file(&path).unwrap(), p);
        assert!(load_from_file(dir.join("absent.ponzik")).unwrap_err().contains("Can't read"));
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
        assert_eq!(p.channels[1].name, "Channel");
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
        assert_eq!(p.tracks[5].name, "Track 5");
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
    fn automation_and_audio_clips_round_trip() {
        let mut p = demo_project();
        let song = AutoScope::Song;
        p.add_automation_point(AutoTarget::Tempo, song, 0, 124.0);
        p.add_automation_point(AutoTarget::Tempo, song, 384, 130.5);
        p.add_automation_point(AutoTarget::TrackGain(3), AutoScope::Pattern(1), 96, 0.25);
        p.add_automation_point(AutoTarget::InstrumentParam { channel: 4, id: 7 }, song, 10, 1_234.5);
        p.add_automation_point(AutoTarget::EffectParam { track: 10, slot: 0, id: 2 }, song, 0, 2.4);
        p.add_automation_point(AutoTarget::ChannelGain(1), song, 0, 0.5);
        p.add_automation_point(AutoTarget::ChannelPan(2), song, 0, -0.75);
        p.add_automation_point(AutoTarget::TrackPan(0), AutoScope::Pattern(2), 5, 0.1);
        p.automation[0].points[0].curve = -0.35;
        p.audio_clips.push(AudioClipDef {
            path: "voix/« refrain ».wav".into(),
            lane: 3,
            track: 7,
            start: 1_536,
            len: 768,
            offset: 0.123_456_789_012,
            gain: 0.8,
            fade_in: 0.01,
            fade_out: 0.25,
            take: 3,
            muted: true,
        });
        let text = save(&p);
        let back = load(&text).unwrap();
        assert_eq!(back, p);
        assert_eq!(save(&back), text);
        // Champs absents (projet d'une version précédente) : vides.
        let old = load(r#"{"format": "ponzik-project", "version": 1}"#).unwrap();
        assert!(old.automation.is_empty() && old.audio_clips.is_empty());
    }

    #[test]
    fn automation_and_audio_clips_are_validated() {
        let text = r#"{
          "format": "ponzik-project",
          "channels": [{}],
          "automation": [
            {"target": {"kind": "channel_gain", "channel": 0},
             "points": [[384, 1], [0, 0.5, 9], ["x"], [96, null], [0, 0.25, -0.5]]},
            {"target": {"kind": "channel_gain", "channel": 0}, "points": [[0, 1]]},
            {"target": {"kind": "channel_pan", "channel": 1}, "points": [[0, 1]]},
            {"target": {"kind": "effect_param", "track": 2, "slot": 8, "id": 0}, "points": [[0, 1]]},
            {"target": {"kind": "track_gain", "track": 99}, "points": [[0, 1]]},
            {"target": {"kind": "tempo"}, "scope": {"pattern": 3}, "points": [[0, 140]]},
            {"target": {"kind": "tempo"}, "scope": {"pattern": 0}, "points": []},
            {"target": {"kind": "inconnu"}, "points": [[0, 1]]},
            {"target": {"kind": "tempo"}, "scope": {"pattern": 0}, "points": [[0, 140]]}
          ],
          "audio_clips": [
            {"path": "a.wav", "track": 50, "len": 0, "offset": -2, "gain": 99, "fade_in": -1},
            {"path": ""}, {"track": 2}
          ]
        }"#;
        let p = load(text).unwrap();
        assert_eq!(p.automation.len(), 2, "{:?}", p.automation);
        let gain = &p.automation[0];
        assert_eq!((gain.target, gain.scope), (AutoTarget::ChannelGain(0), AutoScope::Song));
        assert_eq!(
            gain.points,
            [AutoPoint { tick: 0, value: 0.25, curve: -0.5 }, AutoPoint { tick: 384, value: 1.0, curve: 0.0 }]
        );
        assert_eq!((p.automation[1].target, p.automation[1].scope), (AutoTarget::Tempo, AutoScope::Pattern(0)));
        assert_eq!(p.audio_clips.len(), 1);
        let c = &p.audio_clips[0];
        assert_eq!((c.track, c.len, c.offset, c.gain, c.fade_in), (TRACKS - 1, 1, 0.0, 16.0, 0.0));
        assert_eq!(load(&save(&p)).unwrap(), p);
    }

    #[test]
    fn sidechain_round_trips_and_is_validated() {
        let mut p = demo_project();
        let mut fx = EffectDef::new("compressor");
        fx.sidechain = Some(7);
        p.tracks[4].inserts[3] = Some(fx);
        let text = save(&p);
        let back = load(&text).unwrap();
        assert_eq!(back.tracks[4].inserts[3].as_ref().unwrap().sidechain, Some(7));
        assert_eq!(back, p);
        assert_eq!(save(&back), text);
        // Sans clé, pas de champ : les anciens fichiers restent identiques.
        assert!(!save(&demo_project()).contains("sidechain"));

        let text = r#"{
          "format": "ponzik-project",
          "tracks": [{}, {"inserts": [{"kind": "gate", "sidechain": 1}, {"kind": "gate", "sidechain": 99},
                                      {"kind": "gate", "sidechain": 2}]},
                     {"inserts": [{"kind": "ducker", "sidechain": 3}]}]
        }"#;
        let p = load(text).unwrap();
        let keys = |t: usize| p.tracks[t].inserts.iter().flatten().map(|f| f.sidechain).collect::<Vec<_>>();
        // Sa propre piste ou hors limites : débranchée ; la 2 écoute la 3 sans boucle.
        assert_eq!(keys(1), [None, None, Some(2)]);
        assert_eq!(keys(2), [Some(3)]);

        // Boucle 1 ↔ 2 par les clés : tout est débranché.
        let text = r#"{
          "format": "ponzik-project",
          "tracks": [{}, {"inserts": [{"kind": "gate", "sidechain": 2}]},
                     {"inserts": [{"kind": "gate", "sidechain": 1}]}]
        }"#;
        let p = load(text).unwrap();
        assert!(p.tracks.iter().flat_map(|t| t.inserts.iter().flatten()).all(|f| f.sidechain.is_none()));
        assert!(p.routing_order().is_some());
    }

    #[test]
    fn note_fx_round_trip_and_build() {
        let mut p = demo_project();
        p.channels[1].note_fx =
            vec![Some(NoteFxDef::new("scale").with(0, 9.0)), None, Some(NoteFxDef::new("arpeggiator").with(3, 0.25))];
        p.channels[2].note_fx = vec![None];
        let text = save(&p);
        assert!(text.contains("\"note_fx\""));
        let back = load(&text).unwrap();
        assert_eq!(back, p);
        assert_eq!(save(&back), text);
        // Sans effet de notes, pas de champ : les anciens fichiers restent identiques.
        assert!(!save(&demo_project()).contains("note_fx"));

        let ch = back.make_channel(1, 48_000.0, &|_| None);
        let kinds: Vec<_> = ch.note_fx.iter().map(|fx| fx.as_ref().map(|f| f.kind())).collect();
        assert_eq!(kinds, [Some("scale"), None, Some("arpeggiator"), None]);
        assert_eq!(ch.note_fx[2].as_ref().unwrap().get_param(3), 0.25);

        // Malformés, inconnus, en trop : vides ou ignorés ; un type inconnu ne se construit pas.
        let text = r#"{"format": "ponzik-project", "channels": [{"note_fx": [
          {"kind": "chord", "params": [[0, 3]]}, {"params": []}, "x", {"kind": "lune"}, {"kind": "repeat"}
        ]}]}"#;
        let p = load(text).unwrap();
        let fx = &p.channels[0].note_fx;
        assert_eq!(fx.len(), NOTE_FX_SLOTS);
        assert_eq!(fx[0], Some(NoteFxDef::new("chord").with(0, 3.0)));
        assert_eq!((fx[1].is_none(), fx[2].is_none()), (true, true));
        let ch = p.make_channel(0, 48_000.0, &|_| None);
        assert!(ch.note_fx[0].is_some() && ch.note_fx[3].is_none());
    }

    #[test]
    fn buses_and_vcas_round_trip() {
        let mut p = demo_project();
        let bus = p.add_bus(&[1, 2]).unwrap();
        p.tracks[bus].folded = true;
        let vca = p.add_vca(&[3, 4]).unwrap();
        let text = save(&p);
        assert!(text.contains(r#""kind": "bus""#) && text.contains(r#""folded": true"#) && text.contains(r#""vca": "#));
        let back = load(&text).unwrap();
        assert_eq!(back, p);
        assert_eq!((back.tracks[bus].kind, back.tracks[vca].kind), (TrackKind::Bus, TrackKind::Vca));
        assert_eq!(back.vca_members(vca), [3, 4]);
        // Une piste ordinaire n'écrit aucun des nouveaux champs.
        assert!(!save(&demo_project()).contains(r#""folded""#));
    }

    #[test]
    fn old_and_odd_track_kinds_load_safely() {
        // Fichier d'avant les bus : pistes ordinaires.
        let p = load(r#"{"format": "ponzik-project", "tracks": [{}, {"output": 2}, {"name": "Batterie"}]}"#).unwrap();
        assert!(p.tracks.iter().all(|t| t.kind == TrackKind::Normal && !t.folded && t.vca.is_none()));
        // Type inconnu, repli hors bus, VCA vers soi, vers une piste ordinaire, hors limites, master typé.
        let text = r#"{"format": "ponzik-project", "tracks": [
          {"kind": "bus", "folded": true},
          {"kind": "lune", "folded": true, "vca": 1},
          {"vca": 3}, {"kind": "vca", "vca": 4}, {"kind": "vca"}, {"vca": 99}, {"vca": 4}
        ]}"#;
        let p = load(text).unwrap();
        assert_eq!((p.tracks[MASTER].kind, p.tracks[MASTER].folded), (TrackKind::Normal, false));
        assert_eq!((p.tracks[1].kind, p.tracks[1].folded, p.tracks[1].vca), (TrackKind::Normal, false, None));
        assert_eq!(p.tracks[2].vca, Some(3));
        assert_eq!((p.tracks[3].kind, p.tracks[3].vca), (TrackKind::Vca, None), "un VCA ne suit pas un VCA");
        assert_eq!(p.tracks[5].vca, None);
        assert_eq!(p.tracks[6].vca, Some(4));
        let p = load(r#"{"format": "ponzik-project", "tracks": [{}, {"vca": 2}]}"#).unwrap();
        assert_eq!(p.tracks[1].vca, None, "la piste 2 n'est pas un VCA");
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
        assert!(e.contains("autre-daw") && e.contains("not a Ponzik project"), "{e}");
        assert!(load(r#"{"version": 1}"#).unwrap_err().contains("format"));
        assert!(load("[1, 2]").unwrap_err().contains("not a Ponzik project"));
        let e = load(r#"{"format": "ponzik-project", "version": 2}"#).unwrap_err();
        assert!(e.contains("version 2") && e.contains("update Ponzik"), "{e}");
        let e = load("{\n  \"format\": \"ponzik-project\",\n  \"bpm\": 12O\n}").unwrap_err();
        assert!(e.contains("Unreadable") && e.contains("line 3"), "{e}");
    }

    #[test]
    fn key_and_scale_round_trip() {
        let mut p = demo_project();
        p.key = 9;
        p.scale = crate::ScaleKind::HarmonicMinor;
        let text = save(&p);
        assert!(text.contains("\"scale\": \"harmonic_minor\""));
        assert_eq!(load(&text).unwrap(), p);
        // Champs absents, inconnus ou hors limites : valeurs par défaut.
        let p = load(r#"{"format": "ponzik-project", "key": 14, "scale": "lune"}"#).unwrap();
        assert_eq!((p.key, p.scale), (2, crate::ScaleKind::Chromatic));
        let p = load(r#"{"format": "ponzik-project", "key": -3, "scale": 4}"#).unwrap();
        assert_eq!((p.key, p.scale), (0, crate::ScaleKind::Chromatic));
    }
}
