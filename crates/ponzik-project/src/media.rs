//! Media files of a project: the samples and audio files it refers to, which
//! of them are offline (missing, or in a format not readable yet), and
//! relinking them to files found elsewhere.
//!
//! An offline file keeps its original path in the project: it plays silence
//! until it is located, and is retried every time the project is loaded.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use crate::Project;
use crate::flp::file_name_of;

/// Folders deeper than this under the searched folder are not looked into.
const MAX_DEPTH: usize = 12;
/// Files looked at, at most, in one search (a whole disk is not walked).
const MAX_FILES: usize = 200_000;

impl Project {
    /// Every media file the project refers to, once each, in order: sampler
    /// samples and pad layers, audio clips, takes and session audio clips.
    /// Frozen renders are left out (Ponzik writes them itself).
    pub fn media_paths(&self) -> Vec<String> {
        let paths = self
            .channels
            .iter()
            .flat_map(|c| c.instrument.sample_paths())
            .chain(self.audio_clips.iter().map(|c| &c.path))
            .chain(self.take_groups.iter().flat_map(|g| g.takes.iter().map(|t| &t.clip.path)))
            .chain(self.session_slots.iter().flatten().filter_map(|s| s.audio.as_ref().map(|a| &a.path)));
        let mut out: Vec<String> = Vec::new();
        for p in paths {
            if !out.contains(p) {
                out.push(p.clone());
            }
        }
        out
    }

    /// The media files that `loaded` says are not available.
    pub fn offline_paths(&self, loaded: &dyn Fn(&str) -> bool) -> Vec<String> {
        self.media_paths().into_iter().filter(|p| !loaded(p)).collect()
    }

    /// Points every reference to a key of `map` to its value. Returns the
    /// number of references changed.
    pub fn relink(&mut self, map: &BTreeMap<String, String>) -> usize {
        let mut changed = 0;
        let mut swap = |path: &mut String| {
            if let Some(new) = map.get(path.as_str()) {
                *path = new.clone();
                changed += 1;
            }
        };
        for c in &mut self.channels {
            if let Some(s) = c.instrument.sample.as_mut() {
                swap(s);
            }
            c.instrument.pads.iter_mut().flatten().for_each(&mut swap);
        }
        self.audio_clips.iter_mut().for_each(|c| swap(&mut c.path));
        self.take_groups.iter_mut().flat_map(|g| g.takes.iter_mut()).for_each(|t| swap(&mut t.clip.path));
        self.session_slots.iter_mut().flatten().filter_map(|s| s.audio.as_mut()).for_each(|a| swap(&mut a.path));
        self.pitch_edits.iter_mut().for_each(|d| swap(&mut d.path));
        changed
    }
}

/// File name without its extension.
fn stem(name: &str) -> &str {
    name.rsplit_once('.').map_or(name, |(s, _)| s).trim()
}

/// Looks for the files of `wanted` (paths written on any system) under
/// `root` and its sub-folders: same file name first (case ignored), then
/// same name with another extension (a file converted to WAV, for example),
/// readable formats first. Returns wanted path → found path.
pub fn find_files(root: &Path, wanted: &[String], readable: &dyn Fn(&Path) -> bool) -> BTreeMap<String, String> {
    let mut by_name: HashMap<String, PathBuf> = HashMap::new();
    let mut by_stem: HashMap<String, Vec<PathBuf>> = HashMap::new();
    let mut seen = 0;
    walk(root, 0, &mut seen, &mut |path: &Path| {
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().to_lowercase()) else { return };
        by_stem.entry(stem(&name).to_string()).or_default().push(path.to_path_buf());
        by_name.entry(name).or_insert_with(|| path.to_path_buf());
    });
    let mut found = BTreeMap::new();
    for w in wanted {
        let name = file_name_of(w).to_lowercase();
        if name.is_empty() {
            continue;
        }
        let hit = by_name.get(&name).cloned().or_else(|| {
            let same = by_stem.get(stem(&name))?;
            same.iter().find(|p| readable(p)).or_else(|| same.first()).cloned()
        });
        if let Some(p) = hit {
            found.insert(w.clone(), p.to_string_lossy().into_owned());
        }
    }
    found
}

/// Every file under `dir` (hidden folders skipped), sorted for a stable order.
fn walk(dir: &Path, depth: usize, seen: &mut usize, f: &mut dyn FnMut(&Path)) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for p in paths {
        if *seen >= MAX_FILES {
            return;
        }
        let hidden = p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.'));
        if p.is_dir() {
            if depth < MAX_DEPTH && !hidden {
                walk(&p, depth + 1, seen, f);
            }
        } else {
            *seen += 1;
            f(&p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AudioClipDef, InstrumentDef};

    fn project() -> Project {
        let mut p = Project::empty();
        let mut sampler = InstrumentDef::new("sampler");
        sampler.sample = Some("G:\\sons\\Kick.wav".into());
        p.add_channel("Kick", sampler, 60);
        for (i, path) in ["G:\\sons\\Audio\\Take 1_REC.wav", "C:\\beats\\Beat.mp3", "/here/ok.wav"].iter().enumerate() {
            p.audio_clips.push(AudioClipDef { path: path.to_string(), start: i as u32 * 384, len: 384, ..Default::default() });
        }
        p.audio_clips.push(AudioClipDef { path: "G:\\sons\\Kick.wav".into(), len: 96, ..Default::default() });
        p
    }

    #[test]
    fn offline_paths_and_relink() {
        let mut p = project();
        assert_eq!(p.media_paths().len(), 4, "each file once");
        let offline = p.offline_paths(&|path| path == "/here/ok.wav");
        assert_eq!(offline, ["G:\\sons\\Kick.wav", "G:\\sons\\Audio\\Take 1_REC.wav", "C:\\beats\\Beat.mp3"]);
        let map = BTreeMap::from([("G:\\sons\\Kick.wav".to_string(), "/found/Kick.wav".to_string())]);
        assert_eq!(p.relink(&map), 2, "the sampler and its audio clip");
        assert_eq!(p.channels[0].instrument.sample.as_deref(), Some("/found/Kick.wav"));
        assert_eq!(p.audio_clips[3].path, "/found/Kick.wav");
        // Offline files and relinked ones survive a save / load round trip.
        let back = crate::load(&crate::save(&p)).unwrap();
        assert_eq!(back.audio_clips, p.audio_clips);
        assert_eq!(back.channels[0].instrument.sample, p.channels[0].instrument.sample);
    }

    #[test]
    fn files_are_found_by_name_then_by_stem() {
        let root = std::env::temp_dir().join(format!("ponzik-relink-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for f in ["a/b/c/take 1_rec.WAV", "beats/Beat.wav", "beats/Beat.ogg", ".hidden/Kick.wav", "x/Kick.flac"] {
            let path = root.join(f);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, b"").unwrap();
        }
        let p = project();
        let wanted = p.offline_paths(&|_| false);
        let readable = |p: &Path| p.extension().is_some_and(|e| e == "wav" || e == "flac");
        let found = find_files(&root, &wanted, &readable);
        let at = |f: &str| root.join(f).to_string_lossy().into_owned();
        // Same name, case ignored, three folders down.
        assert_eq!(found.get("G:\\sons\\Audio\\Take 1_REC.wav"), Some(&at("a/b/c/take 1_rec.WAV")));
        // Converted: the readable file with the same name wins.
        assert_eq!(found.get("C:\\beats\\Beat.mp3"), Some(&at("beats/Beat.wav")));
        // Hidden folders are skipped: the FLAC is the match.
        assert_eq!(found.get("G:\\sons\\Kick.wav"), Some(&at("x/Kick.flac")));
        assert!(!found.contains_key("/here/ok.wav"));
        let mut p = p;
        assert_eq!(p.relink(&found), 4);
        assert!(p.offline_paths(&|path| path.starts_with(root.to_str().unwrap()) || path == "/here/ok.wav").is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
