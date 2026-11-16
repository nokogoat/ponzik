//! Fichiers : dossiers de travail, projets, export, échantillons.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ponzik_engine::Sample;
use ponzik_i18n::tr;
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

/// The bundled sound library, rendered at first run.
pub fn library_dir() -> PathBuf {
    projects_dir().join("Library")
}

/// Dossier des réglages : `$XDG_CONFIG_HOME/ponzik` (ou `~/.config/ponzik`) sous
/// Linux, `%APPDATA%\Ponzik` sous Windows, `~/Library/Application Support/Ponzik` sous macOS.
pub fn config_dir() -> PathBuf {
    let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    if cfg!(windows) {
        return var("APPDATA").unwrap_or_else(home).join("Ponzik");
    }
    if cfg!(target_os = "macos") {
        return home().join("Library/Application Support/Ponzik");
    }
    var("XDG_CONFIG_HOME").unwrap_or_else(|| home().join(".config")).join("ponzik")
}

fn file_name(name: &str) -> String {
    let clean: String = name.chars().map(|c| if c.is_alphanumeric() || " -_".contains(c) { c } else { '_' }).collect();
    let clean = clean.trim();
    if clean.is_empty() { tr("Untitled").into() } else { clean.to_string() }
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
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
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
        return Err(tr("The playlist is empty: place patterns before exporting").into());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let mut audio = ponzik_project::render_song(project, sample_rate, loader, RenderMode::Song);
    let gain = ponzik_project::export_wav(path, &mut audio, sample_rate, options)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok((audio.len() as f32 / 2.0 / sample_rate as f32, gain))
}

/// Renders each section (marker to marker) to "<project> - <section>.wav" in
/// the exports folder; returns the folder and the number of files.
pub fn export_regions(
    project: &Project,
    sample_rate: u32,
    loader: &dyn Fn(&str) -> Option<Arc<Sample>>,
    options: ExportOptions,
) -> Result<(PathBuf, usize), String> {
    if project.markers.is_empty() {
        return Err(tr("No sections: add markers on the arrangement lane first").into());
    }
    let dir = projects_dir().join("Exports");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut taken = Vec::new();
    for (i, (name, mut audio)) in ponzik_project::render_regions(project, sample_rate, loader).into_iter().enumerate() {
        let file = ponzik_project::region_file_name(&project.name, &name, i, &taken);
        let path = dir.join(&file);
        ponzik_project::export_wav(&path, &mut audio, sample_rate, options).map_err(|e| format!("{}: {e}", path.display()))?;
        taken.push(file);
    }
    Ok((dir, taken.len()))
}

pub fn load_sample(path: &str) -> Result<Sample, String> {
    ponzik_engine::wav::load_wav(path)
}

/// Cherche des fichiers WAV dans les dossiers habituels. The library comes
/// first, sorted by folder, then everything else by name.
pub fn scan_samples() -> Vec<PathBuf> {
    let h = home();
    let library = library_dir();
    let mut found = Vec::new();
    walk(&library, 0, None, &mut found);
    let order = ponzik_project::library::library_order();
    found.sort_by_cached_key(|p| {
        let rel = p.strip_prefix(&library).unwrap_or(p);
        (order.iter().position(|o| o == rel).unwrap_or(usize::MAX), p.clone())
    });
    let roots = [
        projects_dir().join("Samples"),
        projects_dir().join("Échantillons"),
        music_dir(),
        h.join("Téléchargements"),
        h.join("Downloads"),
    ];
    let mut out = Vec::new();
    for root in roots {
        walk(&root, 0, Some(&library), &mut out);
        if out.len() >= SAMPLE_SCAN_LIMIT {
            break;
        }
    }
    out.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()));
    out.dedup();
    found.extend(out);
    found
}

/// Folder of a library file relative to the library root (`Drums/Kicks`), if it is one.
pub fn library_folder(path: &Path) -> Option<String> {
    let rel = path.parent()?.strip_prefix(library_dir()).ok()?;
    Some(rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/"))
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
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let file = ponzik_project::read_smf(&bytes)?;
    let name = path.file_stem().map_or("MIDI".to_string(), |s| s.to_string_lossy().into_owned());
    Ok(ponzik_project::import_midi(project, &file, &name))
}

/// FL Studio projects in the usual folders (FL's own project folder included).
pub fn scan_flp() -> Vec<PathBuf> {
    let h = home();
    let mut out = Vec::new();
    let fl = h.join("Documents").join("Image-Line").join("FL Studio").join("Projects");
    for root in [projects_dir(), fl, music_dir(), h.join("Téléchargements"), h.join("Downloads")] {
        walk_ext(&root, 0, &["flp"], &mut out);
    }
    out.sort();
    out.dedup();
    out
}

/// Converts an FL Studio project; samples are looked up next to it too.
pub fn import_flp(path: &Path) -> Result<(Project, ponzik_project::ImportReport), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{} : {e}", path.display()))?;
    let name = path.file_stem().map_or("FL Studio".to_string(), |s| s.to_string_lossy().into_owned());
    let dir = path.parent();
    ponzik_project::import_flp(&bytes, &name, &|raw| ponzik_project::resolve_sample(raw, dir))
}

pub fn export_midi(project: &Project, pattern: usize) -> Result<PathBuf, String> {
    let dir = projects_dir().join("Exports");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join(format!("{} - {}.mid", file_name(&project.name), file_name(&project.patterns[pattern].name)));
    std::fs::write(&path, ponzik_project::write_smf(project, pattern)).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

fn walk(dir: &Path, depth: usize, skip: Option<&Path>, out: &mut Vec<PathBuf>) {
    if depth > 4 || out.len() >= SAMPLE_SCAN_LIMIT || skip.is_some_and(|s| s == dir) {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')) {
            continue;
        }
        if p.is_dir() {
            walk(&p, depth + 1, skip, out);
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
        return Err(tr("The playlist is empty: place patterns before exporting").into());
    }
    let dir = projects_dir().join("Exports").join(file_name(&project.name));
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let stems = ponzik_project::render_stems(project, sample_rate, loader, RenderMode::Song);
    for (track, audio) in &stems {
        let name = format!("{:02} {}.wav", track, file_name(&project.tracks[*track].name));
        let path = dir.join(name);
        ponzik_project::write_wav(&path, audio, sample_rate, BitDepth::Pcm24).map_err(|e| format!("{}: {e}", path.display()))?;
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
pub fn save_preset(kind: &str, name: &str, params: &[(u32, f32)], rack: Option<&ponzik_project::RackDef>) -> Result<PathBuf, String> {
    let dir = presets_dir(kind);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join(format!("{}.json", file_name(name)));
    std::fs::write(&path, ponzik_project::presets::rack_preset_json(kind, name, params, rack)).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

pub fn load_preset(path: &Path) -> Result<Vec<(u32, f32)>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    ponzik_project::presets::parse_preset(&text)
}
