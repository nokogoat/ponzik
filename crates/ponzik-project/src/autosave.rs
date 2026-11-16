//! Sauvegarde automatique et reprise après crash.
//!
//! Au démarrage, l'application appelle d'abord [`recoverable`] (un verrou resté
//! d'une session précédente signale un crash), puis [`Autosaver::start`], qui
//! pose son propre verrou. [`Autosaver::tick`] est appelé souvent (à chaque
//! image, par exemple) : il n'écrit que toutes les [`Autosaver::interval`]
//! secondes, et seulement si le projet a changé. [`Autosaver::close`] retire le
//! verrou à la sortie normale.
//!
//! Fichiers : `<dossier>/autosave/<nom du projet>-<AAAAMMJJ-HHMMSS-mmm>.ponzik`
//! (heure UTC), écrits via un fichier temporaire ; on garde les
//! [`Autosaver::keep`] plus récents de chaque projet.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use ponzik_i18n::{tr, tr_fmt};

use crate::{Project, io};

/// Intervalle par défaut entre deux sauvegardes, en secondes.
pub const DEFAULT_INTERVAL: f64 = 120.0;
/// Nombre de sauvegardes gardées par projet, par défaut.
pub const DEFAULT_KEEP: usize = 5;

const SUBDIR: &str = "autosave";
const LOCK: &str = "session.lock";
const EXT: &str = "ponzik";
/// Longueur de `AAAAMMJJ-HHMMSS-mmm`.
const STAMP_LEN: usize = 19;

pub struct Autosaver {
    /// `<dossier>/autosave`.
    dir: PathBuf,
    /// Secondes minimales entre deux écritures.
    pub interval: f64,
    /// Sauvegardes gardées par projet (au moins une).
    pub keep: usize,
    last_check: f64,
    /// Texte du dernier état écrit ou enregistré : rien à faire s'il n'a pas changé.
    last_text: String,
    locked: bool,
}

impl Autosaver {
    /// Ouvre une session dans `dir` : crée `dir/autosave` et y pose le verrou.
    /// `project` est l'état de départ, qui n'est pas réécrit tant qu'il ne change pas.
    pub fn start(dir: impl AsRef<Path>, project: &Project, now: f64) -> Result<Self, String> {
        let dir = dir.as_ref().join(SUBDIR);
        std::fs::create_dir_all(&dir).map_err(|e| tr_fmt!("Can't create \"{}\": {}", dir.display().to_string(), e.to_string()))?;
        let lock = dir.join(LOCK);
        std::fs::write(&lock, format!("pid={}\n", std::process::id()))
            .map_err(|e| tr_fmt!("Can't write \"{}\": {}", lock.display().to_string(), e.to_string()))?;
        Ok(Self {
            dir,
            interval: DEFAULT_INTERVAL,
            keep: DEFAULT_KEEP,
            last_check: now,
            last_text: io::save(project),
            locked: true,
        })
    }

    /// Dossier des sauvegardes automatiques.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Le projet vient d'être enregistré ou chargé : inutile de le sauvegarder
    /// tant qu'il ne change pas.
    pub fn mark_saved(&mut self, project: &Project) {
        self.last_text = io::save(project);
    }

    /// Écrit le projet si l'intervalle est écoulé depuis la dernière vérification
    /// et s'il a changé ; renvoie le fichier écrit. `now` : secondes, horloge
    /// quelconque mais croissante. Après un échec, on réessaie à l'intervalle suivant.
    pub fn tick(&mut self, project: &Project, now: f64) -> Result<Option<PathBuf>, String> {
        if now < self.last_check {
            // Horloge remise à zéro : on repart de maintenant.
            self.last_check = now;
        }
        if now - self.last_check < self.interval {
            return Ok(None);
        }
        self.last_check = now;
        let text = io::save(project);
        if text == self.last_text {
            return Ok(None);
        }
        self.write(&project.name, text).map(Some)
    }

    /// Écrit le projet tout de suite, modifié ou non.
    pub fn save_now(&mut self, project: &Project) -> Result<PathBuf, String> {
        self.write(&project.name, io::save(project))
    }

    fn write(&mut self, name: &str, text: String) -> Result<PathBuf, String> {
        let name = file_name(name);
        let mine: Vec<Entry> = entries(&self.dir).into_iter().filter(|e| e.name == name).collect();
        // Horodatage strictement croissant, même deux fois dans la même milliseconde.
        let ms = mine.last().map_or(0, |e| e.ms + 1).max(now_ms());
        let path = self.dir.join(format!("{name}-{}.{EXT}", format_stamp(ms)));
        write_atomic(&path, &text)?;
        self.last_text = text;
        let excess = (mine.len() + 1).saturating_sub(self.keep.max(1));
        for old in mine.iter().take(excess) {
            let _ = std::fs::remove_file(&old.path);
        }
        Ok(path)
    }

    /// Fin normale de la session : retire le verrou.
    pub fn close(mut self) -> Result<(), String> {
        self.unlock()
    }

    fn unlock(&mut self) -> Result<(), String> {
        if !std::mem::take(&mut self.locked) {
            return Ok(());
        }
        let lock = self.dir.join(LOCK);
        match std::fs::remove_file(&lock) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(tr_fmt!("Can't remove \"{}\": {}", lock.display().to_string(), e.to_string()))
            }
            _ => Ok(()),
        }
    }
}

impl Drop for Autosaver {
    /// Une sortie normale retire le verrou ; une panique le laisse, comme un crash.
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let _ = self.unlock();
        }
    }
}

/// Sauvegarde automatique la plus récente (tous projets confondus) si la
/// session précédente ne s'est pas terminée normalement : son verrou est resté
/// et le processus qui l'a posé ne tourne plus. Hors Linux, on ne sait pas
/// vérifier ce processus : un verrou d'un autre processus compte comme un crash.
pub fn recoverable(dir: impl AsRef<Path>) -> Option<PathBuf> {
    let dir = dir.as_ref().join(SUBDIR);
    if !lock_is_stale(&dir.join(LOCK)) {
        return None;
    }
    entries(&dir).pop().map(|e| e.path)
}

/// Sauvegardes automatiques du dossier, de la plus ancienne à la plus récente.
pub fn autosaves(dir: impl AsRef<Path>) -> Vec<PathBuf> {
    entries(&dir.as_ref().join(SUBDIR)).into_iter().map(|e| e.path).collect()
}

fn lock_is_stale(lock: &Path) -> bool {
    let text = match std::fs::read_to_string(lock) {
        Ok(text) => text,
        Err(e) => return e.kind() != std::io::ErrorKind::NotFound,
    };
    let pid = text.trim().strip_prefix("pid=").and_then(|p| p.parse::<u32>().ok());
    match pid {
        Some(pid) if pid == std::process::id() => false,
        Some(pid) => !process_alive(pid),
        None => true,
    }
}

fn process_alive(pid: u32) -> bool {
    cfg!(target_os = "linux") && Path::new(&format!("/proc/{pid}")).exists()
}

struct Entry {
    name: String,
    ms: u64,
    path: PathBuf,
}

/// Sauvegardes reconnues dans `dir`, triées par date.
fn entries(dir: &Path) -> Vec<Entry> {
    let Ok(read) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<Entry> = read
        .flatten()
        .filter_map(|d| {
            let path = d.path();
            if path.extension()? != EXT {
                return None;
            }
            let stem = path.file_stem()?.to_str()?;
            let (name, ms) = split_stem(stem)?;
            Some(Entry { name: name.to_string(), ms, path })
        })
        .collect();
    out.sort_by(|a, b| (a.ms, &a.name).cmp(&(b.ms, &b.name)));
    out
}

/// `<nom>-<horodatage>` → (nom, millisecondes).
fn split_stem(stem: &str) -> Option<(&str, u64)> {
    let cut = stem.len().checked_sub(STAMP_LEN + 1)?;
    let tail = &stem.as_bytes()[cut..];
    // Fin entièrement ASCII : la coupe tombe sur une frontière de caractère.
    if tail[0] != b'-' || !tail.is_ascii() {
        return None;
    }
    Some((&stem[..cut], parse_stamp(&stem[cut + 1..])?))
}

/// Nom de projet utilisable comme nom de fichier sur tous les systèmes.
fn file_name(name: &str) -> String {
    let clean: String =
        name.trim().chars().take(60).map(|c| if c.is_alphanumeric() || " -_".contains(c) { c } else { '_' }).collect();
    let clean = clean.trim();
    if clean.is_empty() { tr("Untitled").into() } else { clean.into() }
}

fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let fail = |e: std::io::Error| tr_fmt!("Can't write \"{}\": {}", path.display().to_string(), e.to_string());
    std::fs::write(&tmp, text).map_err(fail)?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        fail(e)
    })
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

// --- Horodatage UTC (algorithmes de calendrier de H. Hinnant) ---

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = i64::from((m + 9) % 12);
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Millisecondes Unix → `AAAAMMJJ-HHMMSS-mmm`, qui se trie comme les dates.
fn format_stamp(ms: u64) -> String {
    let secs = ms / 1_000;
    let (y, mo, d) = civil_from_days((secs / 86_400) as i64);
    let s = secs % 86_400;
    format!("{y:04}{mo:02}{d:02}-{:02}{:02}{:02}-{:03}", s / 3_600, s / 60 % 60, s % 60, ms % 1_000)
}

fn parse_stamp(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    let digits = |r: std::ops::Range<usize>| -> Option<u64> {
        let part = s.get(r)?;
        part.bytes().all(|c| c.is_ascii_digit()).then_some(part)?.parse().ok()
    };
    if b.len() != STAMP_LEN || b[8] != b'-' || b[15] != b'-' {
        return None;
    }
    let (y, mo, d) = (digits(0..4)?, digits(4..6)?, digits(6..8)?);
    let (h, mi, sec, ms) = (digits(9..11)?, digits(11..13)?, digits(13..15)?, digits(16..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 59 {
        return None;
    }
    let days = u64::try_from(days_from_civil(y as i64, mo as u32, d as u32)).ok()?;
    Some(((days * 86_400 + h * 3_600 + mi * 60 + sec) * 1_000) + ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Dossier temporaire propre à un test, effacé à la fin.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("ponzik-autosave-{}-{tag}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn project(name: &str, bpm: f32) -> Project {
        Project { name: name.into(), bpm, ..Project::empty() }
    }

    #[test]
    fn writes_on_interval_only_when_modified() {
        let tmp = TempDir::new("interval");
        let mut p = project("Morceau", 120.0);
        let mut saver = Autosaver::start(&tmp.0, &p, 0.0).unwrap();
        assert_eq!(saver.tick(&p, 500.0).unwrap(), None, "inchangé");
        p.bpm = 130.0;
        assert_eq!(saver.tick(&p, 550.0).unwrap(), None, "intervalle pas écoulé");
        let path = saver.tick(&p, 620.0).unwrap().expect("modifié : écrit");
        let file = path.file_name().unwrap().to_str().unwrap();
        assert!(file.starts_with("Morceau-") && file.ends_with(".ponzik"), "{file}");
        assert_eq!(io::load_from_file(&path).unwrap(), p);
        assert_eq!(saver.tick(&p, 1_000.0).unwrap(), None, "déjà écrit");

        // Enregistré à la main : plus rien à sauvegarder.
        p.bpm = 140.0;
        saver.mark_saved(&p);
        assert_eq!(saver.tick(&p, 2_000.0).unwrap(), None);
        // Horloge revenue en arrière : on attend un intervalle complet.
        p.bpm = 150.0;
        assert_eq!(saver.tick(&p, 10.0).unwrap(), None);
        assert!(saver.tick(&p, 130.0).unwrap().is_some());
        assert_eq!(autosaves(&tmp.0).len(), 2);
        assert!(!saver.dir().join("session.lock.tmp").exists());
    }

    #[test]
    fn keeps_the_last_files_of_each_project() {
        let tmp = TempDir::new("keep");
        let mut p = project("Démo: été/2026", 100.0);
        let mut saver = Autosaver::start(&tmp.0, &p, 0.0).unwrap();
        saver.interval = 0.0;
        saver.keep = 3;
        let other = saver.save_now(&project("Autre", 90.0)).unwrap();
        let mut written = Vec::new();
        for i in 0..5 {
            p.bpm = 101.0 + i as f32;
            written.push(saver.tick(&p, i as f64).unwrap().unwrap());
        }
        let files = autosaves(&tmp.0);
        assert_eq!(files.len(), 4);
        assert!(files.contains(&other), "les autres projets ne sont pas élagués");
        assert_eq!(files[1..], written[2..], "les trois plus récentes, dans l'ordre");
        assert!(written[..2].iter().all(|f| !f.exists()));
        let name = written[4].file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with("Démo_ été_2026-"), "{name}");
        assert_eq!(io::load_from_file(&written[4]).unwrap().bpm, 105.0);
        // Aucun fichier temporaire ne traîne.
        let leftovers = std::fs::read_dir(saver.dir())
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "tmp"));
        assert_eq!(leftovers.count(), 0);
    }

    #[test]
    fn stale_lock_offers_the_newest_autosave() {
        let tmp = TempDir::new("lock");
        let lock = tmp.0.join("autosave").join("session.lock");
        assert_eq!(recoverable(&tmp.0), None, "première session");

        // Sortie normale : verrou retiré, rien à reprendre.
        let mut p = project("A", 120.0);
        let mut saver = Autosaver::start(&tmp.0, &p, 0.0).unwrap();
        assert!(lock.exists());
        p.bpm = 121.0;
        saver.save_now(&p).unwrap();
        assert_eq!(recoverable(&tmp.0), None, "session en cours (même processus)");
        saver.close().unwrap();
        assert!(!lock.exists());
        assert_eq!(recoverable(&tmp.0), None);

        // Crash : le verrou reste, posé par un processus disparu.
        let mut saver = Autosaver::start(&tmp.0, &p, 0.0).unwrap();
        p.bpm = 122.0;
        let newest = saver.save_now(&project("B", 122.0)).unwrap();
        std::mem::forget(saver);
        std::fs::write(&lock, format!("pid={}\n", u32::MAX)).unwrap();
        assert_eq!(recoverable(&tmp.0), Some(newest.clone()));
        std::fs::write(&lock, "abîmé").unwrap();
        assert_eq!(recoverable(&tmp.0), Some(newest));

        // La session suivante reprend le verrou à son compte.
        let saver = Autosaver::start(&tmp.0, &p, 0.0).unwrap();
        assert_eq!(recoverable(&tmp.0), None);
        drop(saver);
        assert!(!lock.exists(), "Drop retire aussi le verrou");
    }

    #[test]
    fn stamps_round_trip_and_sort() {
        assert_eq!(format_stamp(0), "19700101-000000-000");
        assert_eq!(format_stamp(951_782_400_000), "20000229-000000-000");
        assert_eq!(format_stamp(1_763_300_000_123), "20251116-133320-123");
        for ms in [0, 1, 999, 86_399_999, 951_782_400_000, 1_763_300_000_123, 4_102_444_800_000] {
            assert_eq!(parse_stamp(&format_stamp(ms)), Some(ms), "{ms}");
        }
        assert!(format_stamp(1_763_300_000_123) < format_stamp(1_763_300_000_124));
        for bad in ["", "20251116-133320", "2025111x-133320-123", "20251316-133320-123", "20251116_133320-123"] {
            assert_eq!(parse_stamp(bad), None, "{bad}");
        }
        assert_eq!(split_stem("Beat-2-20251116-133320-123"), Some(("Beat-2", 1_763_300_000_123)));
        assert_eq!(split_stem("été-20251116-133320-123").map(|s| s.0), Some("été"));
        assert_eq!(split_stem("20251116-133320-123"), None);
        assert_eq!(split_stem("éé"), None);
        assert_eq!(file_name("  "), "Untitled");
        assert_eq!(file_name("a<b>c"), "a_b_c");
    }
}
