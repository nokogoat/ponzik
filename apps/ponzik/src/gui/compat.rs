//! Fichiers : dossiers de travail, projets, export, échantillons.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ponzik_engine::Sample;
use ponzik_i18n::tr;
use ponzik_project::{BitDepth, ExportOptions, FileFormat, Project, RenderMode};

const SAMPLE_SCAN_LIMIT: usize = 400;

pub(super) fn home() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
}

/// Dossier musique de l'utilisateur (« Musique » en français, sinon « Music »).
pub(super) fn music_dir() -> PathBuf {
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

/// The user's translations (`<config>/lang/*.po`); `PONZIK_LANG_DIR` points
/// elsewhere (screenshots, translators testing a file).
pub fn translations_dir() -> PathBuf {
    std::env::var_os("PONZIK_LANG_DIR").filter(|v| !v.is_empty()).map_or_else(|| config_dir().join("lang"), PathBuf::from)
}

/// Shows a folder in the system file manager, without waiting for it.
pub fn open_in_file_manager(path: &Path) -> std::io::Result<()> {
    let program = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let mut child = std::process::Command::new(program)
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    // Reaped in the background: no zombie left behind.
    std::thread::spawn(move || child.wait());
    Ok(())
}

pub(super) fn file_name(name: &str) -> String {
    let clean: String = name.chars().map(|c| if c.is_alphanumeric() || " -_".contains(c) { c } else { '_' }).collect();
    let clean = clean.trim();
    if clean.is_empty() { tr("Untitled").into() } else { clean.to_string() }
}

/// `Exports/<project>.<ext>` (`ext`: "wav" or "flac").
pub fn default_export_path(name: &str, ext: &str) -> PathBuf {
    projects_dir().join("Exports").join(format!("{}.{ext}", file_name(name)))
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

/// Rend tout le morceau en WAV ou FLAC ; renvoie sa durée en secondes et le gain de normalisation.
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
    let gain = ponzik_project::export_audio(path, &mut audio, sample_rate, options)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok((audio.len() as f32 / 2.0 / sample_rate as f32, gain))
}

/// Renders each section (marker to marker) to "<project> - <section>.wav" (or
/// .flac) in the exports folder; returns the folder and the number of files.
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
        let file = ponzik_project::region_file_name(&project.name, &name, i, &taken, options.format.extension());
        let path = dir.join(&file);
        ponzik_project::export_audio(&path, &mut audio, sample_rate, options).map_err(|e| format!("{}: {e}", path.display()))?;
        taken.push(file);
    }
    Ok((dir, taken.len()))
}

/// A WAV file; a library sound whose file is not on disk (yet) is rendered in memory.
pub fn load_sample(path: &str) -> Result<Sample, String> {
    ponzik_engine::wav::load_wav(path).or_else(|e| {
        // A library sound named by its relative path (the demos): the file of
        // the library on disk when it is current, decoded far faster than the
        // sound is rendered again.
        let dir = library_dir();
        let rel = Path::new(path);
        if rel.is_relative() && ponzik_project::library::library_is_current(&dir)
            && let Ok(s) = ponzik_engine::wav::load_wav(dir.join(rel))
        {
            return Ok(s);
        }
        ponzik_project::library::library_sample(path).ok_or(e)
    })
}

/// Every sample a project uses (sampler instruments and audio clips), loaded
/// once; those that fail to load are left out.
pub fn project_samples(project: &Project) -> HashMap<String, Arc<Sample>> {
    let mut samples = HashMap::new();
    let paths = project
        .channels
        .iter()
        .flat_map(|c| c.instrument.sample_paths())
        .chain(project.audio_clips.iter().map(|c| &c.path))
        .chain(project.take_groups.iter().flat_map(|g| g.takes.iter().map(|t| &t.clip.path)))
        .chain(project.freeze_paths());
    let mut unique: Vec<&String> = Vec::new();
    for path in paths {
        if !unique.contains(&path) {
            unique.push(path);
        }
    }
    // Decoded (or rendered, for library sounds not on disk) side by side:
    // a demo's sounds come up in a fraction of the time.
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(unique.len()).max(1);
    let per = unique.len().div_ceil(threads).max(1);
    std::thread::scope(|scope| {
        let jobs: Vec<_> = unique
            .chunks(per)
            .map(|chunk| scope.spawn(move || chunk.iter().filter_map(|p| Some(((*p).clone(), Arc::new(load_sample(p).ok()?)))).collect::<Vec<_>>()))
            .collect();
        for job in jobs {
            samples.extend(job.join().unwrap_or_default());
        }
    });
    samples
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
        // Where older versions saved the Sample Lab's files.
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

/// Opens an Ableton Live set; audio files are looked up next to it too.
pub fn import_als(path: &Path) -> Result<(Project, ponzik_project::ImportReport), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{} : {e}", path.display()))?;
    let name = path.file_stem().map_or("Ableton Live".to_string(), |s| s.to_string_lossy().into_owned());
    let dir = path.parent();
    ponzik_project::als::import_als(&bytes, &name, &|raw| ponzik_project::resolve_sample(raw, dir))
}

/// A sample path of the project as a path on disk (library sounds are
/// relative to the library), for the files other programs read.
fn media_on_disk(raw: &str) -> Option<String> {
    let p = Path::new(raw);
    if p.is_absolute() {
        return Some(raw.to_string());
    }
    let lib = library_dir().join(p);
    lib.is_file().then(|| lib.display().to_string())
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(path, bytes).map_err(|e| format!("{}: {e}", path.display()))
}

/// An FL Studio copy of the project.
pub fn export_flp(project: &Project, path: &Path) -> Result<(), String> {
    write_file(path, &ponzik_project::export_flp(project, &media_on_disk))
}

/// An Ableton Live copy of the project.
pub fn export_als(project: &Project, path: &Path) -> Result<(), String> {
    write_file(path, &ponzik_project::als::export_als(project))
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
        } else if ponzik_engine::wav::is_audio_file(&p) {
            out.push(p);
            if out.len() >= SAMPLE_SCAN_LIMIT {
                return;
            }
        }
    }
}

/// Rend une piste de mixer par fichier dans `Exports/<projet>/` (24-bit WAV or
/// FLAC, never normalized: the stems add up to the mix) ; renvoie le dossier
/// et le nombre de fichiers.
pub fn export_stems(
    project: &Project,
    sample_rate: u32,
    loader: &dyn Fn(&str) -> Option<Arc<Sample>>,
    format: FileFormat,
) -> Result<(PathBuf, usize), String> {
    if project.clips.is_empty() && project.audio_clips.is_empty() && project.take_groups.is_empty() {
        return Err(tr("The playlist is empty: place patterns before exporting").into());
    }
    let dir = projects_dir().join("Exports").join(file_name(&project.name));
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let stems = ponzik_project::render_stems(project, sample_rate, loader, RenderMode::Song);
    for (track, audio) in &stems {
        let name = format!("{:02} {}.{}", track, file_name(&project.tracks[*track].name), format.extension());
        let path = dir.join(name);
        let options = ExportOptions { depth: BitDepth::Pcm24, format, ..Default::default() };
        ponzik_project::write_audio(&path, audio, sample_rate, options).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok((dir, stems.len()))
}

/// Dossier des presets d'un type d'appareil : `Presets/<id du plugin>` pour un
/// plugin CLAP ou VST 3 (ses presets le suivent s'il change de place).
pub fn presets_dir(kind: &str) -> PathBuf {
    projects_dir().join("Presets").join(file_name(ponzik_project::presets::preset_folder(kind)))
}

pub fn list_presets(kind: &str) -> Vec<PathBuf> {
    // Plugin presets saved before were filed under the whole kind.
    let legacy = projects_dir().join("Presets").join(file_name(kind));
    let mut dirs = vec![presets_dir(kind)];
    if !dirs.contains(&legacy) {
        dirs.push(legacy);
    }
    let mut files: Vec<PathBuf> = dirs
        .iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flat_map(|it| it.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|e| e == "json")))
        .collect();
    files.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
    files
}

/// Enregistre les réglages d'un appareil : `{"kind": …, "params": [[id, valeur], …]}`,
/// plus les chaînes d'un rack ou l'état d'un plugin.
pub fn save_preset(
    kind: &str,
    name: &str,
    params: &[(u32, f32)],
    rack: Option<&ponzik_project::RackDef>,
    state: Option<&[u8]>,
) -> Result<PathBuf, String> {
    let dir = presets_dir(kind);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join(format!("{}.json", file_name(name)));
    let text = ponzik_project::presets::device_preset_json(kind, name, params, rack, state);
    std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

/// A preset file: its values, and a rack's chains or a plugin's state.
pub struct LoadedPreset {
    pub params: Vec<(u32, f32)>,
    pub rack: Option<ponzik_project::RackDef>,
    pub state: Option<ponzik_project::PluginState>,
}

pub fn load_preset(path: &Path) -> Result<LoadedPreset, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(LoadedPreset {
        params: ponzik_project::presets::parse_preset(&text)?,
        rack: ponzik_project::presets::parse_rack(&text),
        state: ponzik_project::presets::parse_state(&text),
    })
}
