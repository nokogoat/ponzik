//! Fichiers : dossiers de travail, projets, export, échantillons.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ponzik_engine::Sample;
use ponzik_project::{BitDepth, ExportOptions, Project, RenderMode};

const SAMPLE_SCAN_LIMIT: usize = 400;

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
}

/// Dossier musique de l'utilisateur (« Musique » en français, sinon « Music »).
fn music_dir() -> PathBuf {
    let h = home();
    ["Musique", "Music"].iter().map(|d| h.join(d)).find(|p| p.is_dir()).unwrap_or(h)
}

pub fn projects_dir() -> PathBuf {
    music_dir().join("Ponzik")
}

fn file_name(name: &str) -> String {
    let clean: String = name.chars().map(|c| if c.is_alphanumeric() || " -_".contains(c) { c } else { '_' }).collect();
    let clean = clean.trim();
    if clean.is_empty() { "Sans titre".into() } else { clean.to_string() }
}

pub fn default_project_path(name: &str) -> PathBuf {
    projects_dir().join(format!("{}.ponzik", file_name(name)))
}

pub fn default_export_path(name: &str) -> PathBuf {
    projects_dir().join("Exports").join(format!("{}.wav", file_name(name)))
}

pub fn list_projects() -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(projects_dir())
        .map(|it| {
            it.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|e| e == "ponzik"))
                .collect()
        })
        .unwrap_or_default();
    files.sort_by_key(|p| std::cmp::Reverse(std::fs::metadata(p).and_then(|m| m.modified()).ok()));
    files
}

pub fn save_project(project: &Project, path: &Path) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{} : {e}", dir.display()))?;
    }
    ponzik_project::save_to_file(project, path)
}

pub fn load_project(path: &Path) -> Result<Project, String> {
    ponzik_project::load_from_file(path)
}

/// Rend tout le morceau en WAV 24 bits ; renvoie sa durée en secondes.
pub fn export_song(
    project: &Project,
    sample_rate: u32,
    loader: &dyn Fn(&str) -> Option<Arc<Sample>>,
    path: &Path,
    options: ExportOptions,
) -> Result<(f32, f32), String> {
    if project.clips.is_empty() {
        return Err("La playlist est vide : place des patterns avant d'exporter".into());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{} : {e}", dir.display()))?;
    }
    let mut audio = ponzik_project::render_song(project, sample_rate, loader, RenderMode::Song);
    let gain = ponzik_project::export_wav(path, &mut audio, sample_rate, options)
        .map_err(|e| format!("{} : {e}", path.display()))?;
    Ok((audio.len() as f32 / 2.0 / sample_rate as f32, gain))
}

pub fn load_sample(path: &str) -> Result<Sample, String> {
    ponzik_engine::wav::load_wav(path)
}

/// Cherche des fichiers WAV dans les dossiers habituels.
pub fn scan_samples() -> Vec<PathBuf> {
    let h = home();
    let roots = [
        projects_dir().join("Samples"),
        music_dir(),
        h.join("Téléchargements"),
        h.join("Downloads"),
    ];
    let mut out = Vec::new();
    for root in roots {
        walk(&root, 0, &mut out);
        if out.len() >= SAMPLE_SCAN_LIMIT {
            break;
        }
    }
    out.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()));
    out.dedup();
    out
}

/// Cherche des fichiers MIDI dans les mêmes dossiers que les échantillons.
pub fn scan_midi() -> Vec<PathBuf> {
    let h = home();
    let mut out = Vec::new();
    for root in [projects_dir(), music_dir(), h.join("Téléchargements"), h.join("Downloads")] {
        walk_ext(&root, 0, &["mid", "midi"], &mut out);
    }
    out.sort();
    out.dedup();
    out
}

fn walk_ext(dir: &Path, depth: usize, exts: &[&str], out: &mut Vec<PathBuf>) {
    if depth > 4 || out.len() >= SAMPLE_SCAN_LIMIT {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')) {
            continue;
        }
        if p.is_dir() {
            walk_ext(&p, depth + 1, exts, out);
        } else if p.extension().is_some_and(|x| exts.iter().any(|e| x.eq_ignore_ascii_case(e))) {
            out.push(p);
        }
    }
}

pub fn import_midi(project: &mut Project, path: &Path) -> Result<usize, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{} : {e}", path.display()))?;
    let file = ponzik_project::read_smf(&bytes)?;
    let name = path.file_stem().map_or("MIDI".to_string(), |s| s.to_string_lossy().into_owned());
    Ok(ponzik_project::import_midi(project, &file, &name))
}

pub fn export_midi(project: &Project, pattern: usize) -> Result<PathBuf, String> {
    let dir = projects_dir().join("Exports");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{} : {e}", dir.display()))?;
    let path = dir.join(format!("{} - {}.mid", file_name(&project.name), file_name(&project.patterns[pattern].name)));
    std::fs::write(&path, ponzik_project::write_smf(project, pattern)).map_err(|e| format!("{} : {e}", path.display()))?;
    Ok(path)
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 4 || out.len() >= SAMPLE_SCAN_LIMIT {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')) {
            continue;
        }
        if p.is_dir() {
            walk(&p, depth + 1, out);
        } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("wav")) {
            out.push(p);
            if out.len() >= SAMPLE_SCAN_LIMIT {
                return;
            }
        }
    }
}

/// Rend une piste de mixer par fichier dans `Exports/<projet>/` ; renvoie le dossier et le nombre de fichiers.
pub fn export_stems(
    project: &Project,
    sample_rate: u32,
    loader: &dyn Fn(&str) -> Option<Arc<Sample>>,
) -> Result<(PathBuf, usize), String> {
    if project.clips.is_empty() && project.audio_clips.is_empty() {
        return Err("La playlist est vide : place des patterns avant d'exporter".into());
    }
    let dir = projects_dir().join("Exports").join(file_name(&project.name));
    std::fs::create_dir_all(&dir).map_err(|e| format!("{} : {e}", dir.display()))?;
    let stems = ponzik_project::render_stems(project, sample_rate, loader, RenderMode::Song);
    for (track, audio) in &stems {
        let name = format!("{:02} {}.wav", track, file_name(&project.tracks[*track].name));
        let path = dir.join(name);
        ponzik_project::write_wav(&path, audio, sample_rate, BitDepth::Pcm24).map_err(|e| format!("{} : {e}", path.display()))?;
    }
    Ok((dir, stems.len()))
}

/// Dossier des presets d'un type d'appareil.
pub fn presets_dir(kind: &str) -> PathBuf {
    projects_dir().join("Presets").join(file_name(kind))
}

pub fn list_presets(kind: &str) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(presets_dir(kind))
        .map(|it| it.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|e| e == "json")).collect())
        .unwrap_or_default();
    files.sort();
    files
}

/// Enregistre les réglages d'un appareil : `{"kind": …, "params": [[id, valeur], …]}`.
pub fn save_preset(kind: &str, name: &str, params: &[(u32, f32)]) -> Result<PathBuf, String> {
    use ponzik_project::json::Json;
    let dir = presets_dir(kind);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{} : {e}", dir.display()))?;
    let path = dir.join(format!("{}.json", file_name(name)));
    let doc = Json::Obj(vec![
        ("kind".into(), Json::Str(kind.into())),
        ("name".into(), Json::Str(name.into())),
        (
            "params".into(),
            Json::Arr(params.iter().map(|&(id, v)| Json::Arr(vec![Json::Num(id as f64), Json::Num(v as f64)])).collect()),
        ),
    ]);
    std::fs::write(&path, doc.pretty()).map_err(|e| format!("{} : {e}", path.display()))?;
    Ok(path)
}

pub fn load_preset(path: &Path) -> Result<Vec<(u32, f32)>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{} : {e}", path.display()))?;
    let doc = ponzik_project::json::parse(&text)?;
    Ok(doc
        .get("params")
        .and_then(|p| p.as_arr())
        .unwrap_or(&[])
        .iter()
        .filter_map(|pair| {
            let a = pair.as_arr()?;
            Some((a.first()?.as_f64()? as u32, a.get(1)?.as_f64()? as f32))
        })
        .collect())
}
