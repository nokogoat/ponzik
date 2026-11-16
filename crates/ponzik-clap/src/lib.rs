//! Hôte de plugins CLAP pour Ponzik : audio, paramètres et notes (pas encore
//! d'interface graphique de plugin).
//!
//! L'ABI C est écrite à la main dans [`abi`] ; les bibliothèques sont chargées
//! avec `dlopen` (Unix) ou `LoadLibraryExW` (Windows). Seuls les `.clap` qui
//! sont des bibliothèques partagées (Linux, Windows) sont pris en charge, pas
//! les paquets macOS.
//!
//! Identifiant d'instrument ou d'effet dans un projet : `clap:<chemin>#<id>`
//! (voir [`kind_for`]), ce qui évite de toucher au format de fichier. C'est aussi
//! ce que renvoient `Instrument::kind` et `Effect::kind`.

pub mod abi;
mod library;
mod plugin;

use std::path::{Path, PathBuf};

use ponzik_engine::{Effect, Instrument};

use crate::library::{Library, c_str};
pub use crate::plugin::{ClapEffect, ClapInstrument, MAX_BLOCK};

/// Préfixe des identifiants de plugins CLAP dans un projet.
pub const KIND_PREFIX: &str = "clap:";

/// Fiche d'un plugin trouvé lors du scan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginInfo {
    pub id: String,
    pub name: String,
    pub vendor: String,
    /// Fichier `.clap` qui contient le plugin.
    pub path: PathBuf,
    /// Le descripteur annonce la fonction « instrument ».
    pub is_instrument: bool,
}

impl PluginInfo {
    /// Identifiant à enregistrer dans le projet.
    pub fn kind(&self) -> String {
        kind_for(&self.path, &self.id)
    }
}

/// `clap:<chemin>#<id>` (chemin converti en UTF-8 avec perte éventuelle).
pub fn kind_for(path: &Path, id: &str) -> String {
    format!("{KIND_PREFIX}{}#{id}", path.display())
}

/// Inverse de [`kind_for`] : `(chemin, id)`, coupé au dernier `#`.
pub fn parse_kind(kind: &str) -> Option<(&Path, &str)> {
    let (path, id) = kind.strip_prefix(KIND_PREFIX)?.rsplit_once('#')?;
    (!path.is_empty() && !id.is_empty()).then_some((Path::new(path), id))
}

/// Dossiers de recherche standard : `$CLAP_PATH` d'abord, puis ceux de la plateforme.
pub fn search_paths() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> =
        std::env::var_os("CLAP_PATH").map(|v| std::env::split_paths(&v).collect()).unwrap_or_default();
    let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
    if cfg!(windows) {
        dirs.extend(env("COMMONPROGRAMFILES").map(|p| p.join("CLAP")));
        dirs.extend(env("LOCALAPPDATA").map(|p| p.join("Programs").join("Common").join("CLAP")));
    } else {
        dirs.extend(env("HOME").map(|p| p.join(".clap")));
        dirs.push("/usr/lib/clap".into());
        dirs.push("/usr/local/lib/clap".into());
    }
    dirs
}

/// Tous les plugins des dossiers standard (les fichiers illisibles sont ignorés).
pub fn scan() -> Vec<PluginInfo> {
    let mut files = Vec::new();
    for dir in search_paths() {
        collect(&dir, 0, &mut files);
    }
    plugins_of(files)
}

/// Plugins des fichiers `.clap` d'un dossier (récursif).
pub fn scan_dir(dir: &Path) -> Vec<PluginInfo> {
    let mut files = Vec::new();
    collect(dir, 0, &mut files);
    plugins_of(files)
}

fn plugins_of(mut files: Vec<PathBuf>) -> Vec<PluginInfo> {
    let mut seen = Vec::new();
    files.retain(|f| {
        let c = f.canonicalize().unwrap_or_else(|_| f.clone());
        let new = !seen.contains(&c);
        seen.push(c);
        new
    });
    files.iter().filter_map(|f| scan_file(f).ok()).flatten().collect()
}

fn collect(dir: &Path, depth: u32, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let path = e.path();
        let is_clap = path.extension().is_some_and(|x| x.eq_ignore_ascii_case("clap"));
        if path.is_file() {
            if is_clap {
                out.push(path);
            }
        } else if path.is_dir() && depth < 8 {
            collect(&path, depth + 1, out);
        }
    }
}

/// Descripteurs de tous les plugins d'un fichier `.clap`.
pub fn scan_file(path: &Path) -> Result<Vec<PluginInfo>, String> {
    let lib = Library::load(path)?;
    let factory = lib.factory().ok_or_else(|| format!("{}: no plugin factory", path.display()))?;
    let mut out = Vec::new();
    // SAFETY : fabrique valide tant que `lib` est chargée ; descripteurs vérifiés.
    for i in 0..unsafe { (factory.get_plugin_count)(factory) } {
        let Some(d) = (unsafe { (factory.get_plugin_descriptor)(factory, i).as_ref() }) else { continue };
        let mut is_instrument = false;
        let mut f = d.features;
        while !f.is_null() && !unsafe { *f }.is_null() {
            is_instrument |= c_str(unsafe { *f }) == "instrument";
            f = unsafe { f.add(1) };
        }
        let id = c_str(d.id);
        if id.is_empty() {
            continue;
        }
        out.push(PluginInfo {
            id,
            name: c_str(d.name),
            vendor: c_str(d.vendor),
            path: path.to_path_buf(),
            is_instrument,
        });
    }
    Ok(out)
}

/// Instancie l'instrument `id` du fichier `path`.
pub fn create_instrument(path: &Path, id: &str) -> Result<Box<dyn Instrument>, String> {
    Ok(Box::new(ClapInstrument::new(path, id)?))
}

/// Instancie l'effet `id` du fichier `path`.
pub fn create_effect(path: &Path, id: &str) -> Result<Box<dyn Effect>, String> {
    Ok(Box::new(ClapEffect::new(path, id)?))
}

/// Instrument depuis un identifiant `clap:<chemin>#<id>`.
pub fn instrument_from_kind(kind: &str) -> Result<Box<dyn Instrument>, String> {
    let (path, id) = parse_kind(kind).ok_or_else(|| format!("identifiant CLAP invalide : {kind}"))?;
    create_instrument(path, id)
}

/// Effet depuis un identifiant `clap:<chemin>#<id>`.
pub fn effect_from_kind(kind: &str) -> Result<Box<dyn Effect>, String> {
    let (path, id) = parse_kind(kind).ok_or_else(|| format!("identifiant CLAP invalide : {kind}"))?;
    create_effect(path, id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_roundtrip() {
        let k = kind_for(Path::new("/opt/a#b/x.clap"), "com.vendor.synth");
        assert_eq!(k, "clap:/opt/a#b/x.clap#com.vendor.synth");
        assert_eq!(parse_kind(&k), Some((Path::new("/opt/a#b/x.clap"), "com.vendor.synth")));
        assert_eq!(parse_kind("pulse"), None);
        assert_eq!(parse_kind("clap:sans-id"), None);
    }

    #[test]
    fn missing_file_is_an_error() {
        assert!(create_effect(Path::new("/nonexistent/x.clap"), "a").is_err());
        assert!(scan_dir(Path::new("/nonexistent")).is_empty());
    }
}
