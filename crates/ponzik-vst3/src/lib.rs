//! Hôte de plugins VST 3 pour Ponzik : audio, paramètres et notes (pas
//! d'interface graphique de plugin).
//!
//! L'ABI COM de VST 3 est écrite à la main dans [`abi`] ; les modules sont
//! chargés avec `dlopen` (Unix) ou `LoadLibraryExW` (Windows). Les paquets
//! `.vst3` (dossiers `Contents/<arch>-linux` ou `Contents/<arch>-win`) et les
//! `.vst3` en un seul fichier (Windows) sont pris en charge, pas ceux de macOS.
//!
//! Identifiant d'instrument ou d'effet dans un projet :
//! `vst3:<chemin du paquet>#<identifiant de classe>` (voir [`kind_for`]).
//! L'identifiant de classe est écrit en 32 chiffres hexadécimaux, sous la forme
//! des quatre mots de `INLINE_UID` (identique sur toutes les plateformes).

pub mod abi;
mod com;
mod library;
mod plugin;

use std::path::{Path, PathBuf};

use ponzik_engine::{Effect, Instrument};

use crate::abi::*;
use crate::library::Module;
pub use crate::plugin::{MAX_BLOCK, Vst3Effect, Vst3Instrument};

/// Préfixe des identifiants de plugins VST 3 dans un projet.
pub const KIND_PREFIX: &str = "vst3:";

/// Fiche d'une classe audio trouvée lors du scan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginInfo {
    /// Identifiant de classe en hexadécimal (voir [`class_id_hex`]).
    pub id: String,
    pub name: String,
    pub vendor: String,
    /// Sous-catégories VST 3 (`Fx|Delay`, `Instrument|Synth`…).
    pub categories: String,
    /// Paquet ou fichier `.vst3` qui contient le plugin.
    pub path: PathBuf,
    /// Les sous-catégories contiennent « Instrument ».
    pub is_instrument: bool,
}

impl PluginInfo {
    /// Identifiant à enregistrer dans le projet.
    pub fn kind(&self) -> String {
        kind_for(&self.path, &self.id)
    }
}

/// Identifiant de classe → 32 chiffres hexadécimaux majuscules (`l1 l2 l3 l4`).
pub fn class_id_hex(cid: &Tuid) -> String {
    let [a, b, c, d] = uid_words(cid);
    format!("{a:08X}{b:08X}{c:08X}{d:08X}")
}

/// Inverse de [`class_id_hex`] (casse indifférente).
pub fn parse_class_id(hex: &str) -> Option<Tuid> {
    if hex.len() != 32 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let w = |i: usize| u32::from_str_radix(&hex[i * 8..i * 8 + 8], 16).ok();
    Some(uid(w(0)?, w(1)?, w(2)?, w(3)?))
}

/// `vst3:<chemin>#<id>` (chemin converti en UTF-8 avec perte éventuelle).
pub fn kind_for(path: &Path, class_id: &str) -> String {
    format!("{KIND_PREFIX}{}#{class_id}", path.display())
}

/// Inverse de [`kind_for`] : `(chemin, id)`, coupé au dernier `#`.
pub fn parse_kind(kind: &str) -> Option<(&Path, &str)> {
    let (path, id) = kind.strip_prefix(KIND_PREFIX)?.rsplit_once('#')?;
    (!path.is_empty() && parse_class_id(id).is_some()).then_some((Path::new(path), id))
}

/// Dossiers de recherche standard : `$VST3_PATH` d'abord, puis ceux de la plateforme.
pub fn search_paths() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> =
        std::env::var_os("VST3_PATH").map(|v| std::env::split_paths(&v).collect()).unwrap_or_default();
    let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
    if cfg!(windows) {
        dirs.extend(env("COMMONPROGRAMFILES").map(|p| p.join("VST3")));
        dirs.extend(env("LOCALAPPDATA").map(|p| p.join("Programs").join("Common").join("VST3")));
    } else {
        dirs.extend(env("HOME").map(|p| p.join(".vst3")));
        dirs.push("/usr/lib/vst3".into());
        dirs.push("/usr/local/lib/vst3".into());
    }
    dirs
}

/// Toutes les classes audio des dossiers standard (les modules illisibles sont ignorés).
pub fn scan() -> Vec<PluginInfo> {
    let mut found = Vec::new();
    for dir in search_paths() {
        collect(&dir, 0, &mut found);
    }
    plugins_of(found)
}

/// Classes audio des `.vst3` d'un dossier (récursif).
pub fn scan_dir(dir: &Path) -> Vec<PluginInfo> {
    let mut found = Vec::new();
    collect(dir, 0, &mut found);
    plugins_of(found)
}

fn plugins_of(mut bundles: Vec<PathBuf>) -> Vec<PluginInfo> {
    let mut seen = Vec::new();
    bundles.retain(|f| {
        let c = f.canonicalize().unwrap_or_else(|_| f.clone());
        let new = !seen.contains(&c);
        seen.push(c);
        new
    });
    bundles.iter().filter_map(|f| scan_bundle(f).ok()).flatten().collect()
}

fn collect(dir: &Path, depth: u32, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let path = e.path();
        if path.extension().is_some_and(|x| x.eq_ignore_ascii_case("vst3")) {
            // Un paquet est un dossier : on ne descend pas dedans.
            out.push(path);
        } else if path.is_dir() && depth < 8 {
            collect(&path, depth + 1, out);
        }
    }
}

/// Classes audio (`Audio Module Class`) d'un paquet ou fichier `.vst3`.
pub fn scan_bundle(path: &Path) -> Result<Vec<PluginInfo>, String> {
    let module = Module::load(path)?;
    let f = module.factory();
    // SAFETY : fabrique vivante tant que `module` l'est ; structures locales.
    let vendor = unsafe {
        let mut info: PFactoryInfo = std::mem::zeroed();
        let ok = (f.vtbl::<IPluginFactoryVtbl>().get_factory_info)(f.as_ptr(), &mut info) == K_RESULT_OK;
        if ok { read_cstr(&info.vendor) } else { String::new() }
    };
    let count = unsafe { (f.vtbl::<IPluginFactoryVtbl>().count_classes)(f.as_ptr()) }.max(0);
    let mut out = Vec::new();
    for i in 0..count {
        let Some(mut p) = class_info(&module, i) else { continue };
        if p.1 != K_VST_AUDIO_EFFECT_CLASS {
            continue;
        }
        if p.0.vendor.is_empty() {
            p.0.vendor = vendor.clone();
        }
        p.0.path = path.to_path_buf();
        out.push(p.0);
    }
    Ok(out)
}

/// Fiche de la classe `i` et sa catégorie, par la fabrique la plus récente disponible.
fn class_info(module: &Module, i: i32) -> Option<(PluginInfo, String)> {
    let mut p = PluginInfo {
        id: String::new(),
        name: String::new(),
        vendor: String::new(),
        categories: String::new(),
        path: PathBuf::new(),
        is_instrument: false,
    };
    let category;
    // SAFETY : fabriques vivantes tant que `module` l'est ; structures C locales sans invariant.
    unsafe {
        if let Some(f3) = module.factory3()
            && let mut w = std::mem::zeroed::<PClassInfoW>()
            && (f3.vtbl::<IPluginFactory3Vtbl>().get_class_info_unicode)(f3.as_ptr(), i, &mut w) == K_RESULT_OK
        {
            (p.id, p.name, p.vendor) = (class_id_hex(&w.cid), read_wstr(&w.name), read_wstr(&w.vendor));
            p.categories = read_cstr(&w.sub_categories);
            category = read_cstr(&w.category);
        } else if let Some(f2) = module.factory2()
            && let mut c = std::mem::zeroed::<PClassInfo2>()
            && (f2.vtbl::<IPluginFactory2Vtbl>().get_class_info2)(f2.as_ptr(), i, &mut c) == K_RESULT_OK
        {
            (p.id, p.name, p.vendor) = (class_id_hex(&c.cid), read_cstr(&c.name), read_cstr(&c.vendor));
            p.categories = read_cstr(&c.sub_categories);
            category = read_cstr(&c.category);
        } else {
            let f = module.factory();
            let mut c = std::mem::zeroed::<PClassInfo>();
            if (f.vtbl::<IPluginFactoryVtbl>().get_class_info)(f.as_ptr(), i, &mut c) != K_RESULT_OK {
                return None;
            }
            (p.id, p.name) = (class_id_hex(&c.cid), read_cstr(&c.name));
            category = read_cstr(&c.category);
        }
    }
    p.is_instrument = p.categories.split('|').any(|c| c.eq_ignore_ascii_case("Instrument"));
    Some((p, category))
}

/// Instancie l'instrument de classe `class_id` du paquet `path`.
pub fn create_instrument(path: &Path, class_id: &str) -> Result<Box<dyn Instrument>, String> {
    Ok(Box::new(Vst3Instrument::new(path, class_id)?))
}

/// Instancie l'effet de classe `class_id` du paquet `path`.
pub fn create_effect(path: &Path, class_id: &str) -> Result<Box<dyn Effect>, String> {
    Ok(Box::new(Vst3Effect::new(path, class_id)?))
}

/// Instrument depuis un identifiant `vst3:<chemin>#<id>`.
pub fn instrument_from_kind(kind: &str) -> Result<Box<dyn Instrument>, String> {
    let (path, id) = parse_kind(kind).ok_or_else(|| format!("identifiant VST 3 invalide : {kind}"))?;
    create_instrument(path, id)
}

/// Effet depuis un identifiant `vst3:<chemin>#<id>`.
pub fn effect_from_kind(kind: &str) -> Result<Box<dyn Effect>, String> {
    let (path, id) = parse_kind(kind).ok_or_else(|| format!("identifiant VST 3 invalide : {kind}"))?;
    create_effect(path, id)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "0123456789ABCDEF0011223344556677";

    #[test]
    fn class_id_roundtrip() {
        let cid = parse_class_id(ID).unwrap();
        assert_eq!(uid_words(&cid), [0x01234567, 0x89ABCDEF, 0x00112233, 0x44556677]);
        assert_eq!(class_id_hex(&cid), ID);
        assert_eq!(parse_class_id(&ID.to_lowercase()), Some(cid));
        assert_eq!(parse_class_id("123"), None);
        assert_eq!(parse_class_id("0123456789ABCDEF001122334455667G"), None);
    }

    #[test]
    fn kind_roundtrip() {
        let k = kind_for(Path::new("/opt/a#b/x.vst3"), ID);
        assert_eq!(k, format!("vst3:/opt/a#b/x.vst3#{ID}"));
        assert_eq!(parse_kind(&k), Some((Path::new("/opt/a#b/x.vst3"), ID)));
        assert_eq!(parse_kind("pulse"), None);
        assert_eq!(parse_kind("vst3:sans-id"), None);
        assert_eq!(parse_kind("vst3:/x.vst3#pas-hexa"), None);
    }

    #[test]
    fn missing_bundle_is_an_error() {
        assert!(create_effect(Path::new("/nonexistent/x.vst3"), ID).is_err());
        assert!(create_instrument(Path::new("/nonexistent/x.vst3"), "zz").is_err());
        assert!(scan_dir(Path::new("/nonexistent")).is_empty());
    }
}
