//! Available languages: English (the source), the catalogs embedded at build
//! time (`lang/*.po`) and the user's own catalogs (`<config>/lang/*.po`),
//! loaded at run time and reloadable without restarting.
//!
//! A user file with the code of an embedded catalog completes or corrects it
//! (someone fixing a French wording does not have to translate everything).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::{Catalog, Lang};

mod embedded {
    include!(concat!(env!("OUT_DIR"), "/catalogs.rs"));
}

/// One language of the picker.
#[derive(Debug)]
pub struct LangInfo {
    pub lang: Lang,
    /// Name of the language in that language (« Español »).
    pub name: String,
    /// Translations (None for English without a user file).
    pub catalog: Option<Catalog>,
    /// Shipped with Ponzik.
    pub embedded: bool,
    /// User file that adds or corrects this language.
    pub file: Option<PathBuf>,
    /// Share of the application's strings translated, 0 to 1.
    pub completion: f32,
    /// Problems found in the user file (shown as a tooltip).
    pub errors: Vec<String>,
}

#[derive(Debug, Default)]
pub struct Registry {
    pub langs: Vec<LangInfo>,
    /// Files skipped entirely (unreadable folder…).
    pub errors: Vec<String>,
}

/// Embedded catalogs, parsed once: (code, catalog).
pub(crate) fn embedded() -> &'static [(&'static str, Catalog)] {
    static PARSED: OnceLock<Vec<(&'static str, Catalog)>> = OnceLock::new();
    PARSED.get_or_init(|| embedded::CATALOGS.iter().map(|&(code, src)| (code, Catalog::parse(src))).collect())
}

/// Every key of the application, as listed by the embedded catalogs (the
/// reference for completion percentages and translation templates).
pub fn reference_keys() -> &'static [String] {
    static KEYS: OnceLock<Vec<String>> = OnceLock::new();
    KEYS.get_or_init(|| {
        let mut seen = std::collections::HashSet::new();
        embedded().iter().flat_map(|(_, c)| c.ids()).filter(|k| seen.insert(*k)).map(str::to_string).collect()
    })
}

/// `fr`, `pt-br`, `pt_BR.UTF-8`, `sr@latin` → `fr`, `pt_BR`, `pt_BR`, `sr`.
pub fn normalize_code(code: &str) -> String {
    let code = code.trim().split(['.', '@']).next().unwrap_or("");
    let mut parts = code.splitn(2, ['_', '-']);
    let lang = parts.next().unwrap_or("").to_ascii_lowercase();
    match parts.next().filter(|r| !r.is_empty()) {
        Some(region) if region.len() <= 3 => format!("{lang}_{}", region.to_ascii_uppercase()),
        Some(region) => format!("{lang}_{region}"),
        None => lang,
    }
}

/// A language code Ponzik accepts for a new translation: `es`, `pt_BR`, `fil`.
pub fn valid_code(code: &str) -> bool {
    let mut parts = code.splitn(2, '_');
    let lang = parts.next().unwrap_or("");
    let lang_ok = (2..=3).contains(&lang.len()) && lang.bytes().all(|b| b.is_ascii_lowercase());
    let region_ok = parts.next().is_none_or(|r| (2..=8).contains(&r.len()) && r.bytes().all(|b| b.is_ascii_alphanumeric()));
    lang_ok && region_ok
}

/// Interned language codes: `Lang` is an index here, stable across reloads.
fn codes() -> &'static Mutex<Vec<&'static str>> {
    static CODES: OnceLock<Mutex<Vec<&'static str>>> = OnceLock::new();
    CODES.get_or_init(|| Mutex::new(vec!["en"]))
}

pub(crate) fn intern(code: &str) -> Lang {
    let mut codes = codes().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(i) = codes.iter().position(|c| *c == code) {
        return Lang(i);
    }
    codes.push(Box::leak(code.to_string().into_boxed_str()));
    Lang(codes.len() - 1)
}

pub(crate) fn code_of(lang: Lang) -> &'static str {
    let codes = codes().lock().unwrap_or_else(|e| e.into_inner());
    codes.get(lang.0).copied().unwrap_or("en")
}

static CURRENT: AtomicPtr<Registry> = AtomicPtr::new(std::ptr::null_mut());

/// The languages in use. Each reload leaks the previous list (a few hundred
/// kilobytes, only when the user asks): `tr` hands out `&'static str`.
pub fn registry() -> &'static Registry {
    let p = CURRENT.load(Ordering::Acquire);
    if !p.is_null() {
        // SAFETY: only ever set from `Box::into_raw` and never freed.
        return unsafe { &*p };
    }
    let fresh = Box::into_raw(Box::new(build(user_dir().as_deref())));
    match CURRENT.compare_exchange(std::ptr::null_mut(), fresh, Ordering::AcqRel, Ordering::Acquire) {
        // SAFETY: as above.
        Ok(_) => unsafe { &*fresh },
        Err(other) => {
            // SAFETY: `fresh` was never shared.
            drop(unsafe { Box::from_raw(fresh) });
            // SAFETY: as above.
            unsafe { &*other }
        }
    }
}

fn install(reg: Registry) -> &'static Registry {
    let p = Box::into_raw(Box::new(reg));
    CURRENT.store(p, Ordering::Release);
    // SAFETY: just created, never freed.
    unsafe { &*p }
}

fn user_dir_slot() -> &'static Mutex<Option<PathBuf>> {
    static DIR: Mutex<Option<PathBuf>> = Mutex::new(None);
    &DIR
}

/// Folder of the user's translations (`<config>/lang`), set by the application.
pub fn user_dir() -> Option<PathBuf> {
    user_dir_slot().lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Sets the user's translation folder and loads its catalogs.
pub fn set_user_dir(dir: Option<PathBuf>) -> &'static Registry {
    *user_dir_slot().lock().unwrap_or_else(|e| e.into_inner()) = dir;
    reload()
}

/// Reads the user's catalogs again (after editing a file): effective at once.
pub fn reload() -> &'static Registry {
    install(build(user_dir().as_deref()))
}

/// Embedded languages plus the `.po` files of `dir`.
pub fn build(dir: Option<&Path>) -> Registry {
    let mut reg = Registry::default();
    reg.langs.push(LangInfo {
        lang: Lang::EN,
        name: "English".into(),
        catalog: None,
        embedded: true,
        file: None,
        completion: 1.0,
        errors: Vec::new(),
    });
    for (code, cat) in embedded() {
        reg.langs.push(LangInfo {
            lang: intern(&normalize_code(code)),
            name: String::new(),
            catalog: Some(cat.clone()),
            embedded: true,
            file: None,
            completion: 0.0,
            errors: Vec::new(),
        });
    }
    if let Some(dir) = dir {
        load_dir(&mut reg, dir);
    }
    let keys = reference_keys();
    for info in &mut reg.langs {
        let code = info.lang.code();
        if let Some(cat) = &info.catalog {
            info.name = cat.name.clone().or_else(|| crate::known::native_name(code).map(str::to_string)).unwrap_or_else(|| code.to_string());
            if info.lang != Lang::EN {
                let done = keys.iter().filter(|k| cat.contains(k)).count();
                info.completion = if keys.is_empty() { 1.0 } else { done as f32 / keys.len() as f32 };
            }
        }
    }
    // English first, then by name.
    reg.langs[1..].sort_by_key(|l| l.name.to_lowercase());
    reg
}

fn load_dir(reg: &mut Registry, dir: &Path) {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            reg.errors.push(format!("{}: {e}", dir.display()));
            return;
        }
    };
    let mut files: Vec<PathBuf> =
        rd.filter_map(Result::ok).map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("po")) && p.is_file()).collect();
    files.sort();
    for path in files {
        let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let (cat, mut errors) = match std::fs::read(&path) {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(text) => (Catalog::parse(&text), Vec::new()),
                Err(e) => {
                    let text = String::from_utf8_lossy(e.as_bytes()).into_owned();
                    (Catalog::parse(&text), vec!["the file is not UTF-8: save it as UTF-8".to_string()])
                }
            },
            Err(e) => (Catalog::default(), vec![format!("cannot read the file: {e}")]),
        };
        errors.extend(cat.errors.iter().cloned());
        // The `Language` header wins over the file name.
        let code = normalize_code(cat.language.as_deref().filter(|c| !c.trim().is_empty()).unwrap_or(&stem));
        if code.is_empty() {
            reg.errors.push(format!("{}: no language code", path.display()));
            continue;
        }
        let lang = intern(&code);
        match reg.langs.iter_mut().find(|l| l.lang == lang) {
            Some(info) => {
                let base = info.catalog.take().unwrap_or_default();
                let merged = base.overlay(&cat);
                if let Some(prev) = &info.file {
                    errors.push(format!("{code} is also translated by {}", prev.display()));
                }
                info.catalog = Some(merged);
                info.file = Some(path);
                info.errors.extend(errors);
            }
            None => reg.langs.push(LangInfo {
                lang,
                name: String::new(),
                catalog: Some(cat),
                embedded: false,
                file: Some(path),
                completion: 0.0,
                errors,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ponzik-i18n-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("temp dir");
        d
    }

    #[test]
    fn codes_are_normalized() {
        assert_eq!(normalize_code("fr_FR.UTF-8"), "fr_FR");
        assert_eq!(normalize_code("pt-br"), "pt_BR");
        assert_eq!(normalize_code("ES"), "es");
        assert_eq!(normalize_code("sr@latin"), "sr");
        assert!(valid_code("es") && valid_code("pt_BR") && valid_code("fil"));
        assert!(!valid_code("") && !valid_code("e") && !valid_code("Español") && !valid_code("es_"));
    }

    #[test]
    fn user_catalogs_add_and_override_languages() {
        let dir = temp_dir("load");
        std::fs::write(
            dir.join("xx.po"),
            "msgid \"\"\nmsgstr \"\"\n\"Language: xx\\n\"\n\"Language-Name: Xxish\\n\"\n\nmsgid \"Cancel\"\nmsgstr \"Xcancel\"\n",
        )
        .expect("write");
        // Language from the header, not the file name; no name → the code.
        std::fs::write(dir.join("whatever.po"), "msgid \"\"\nmsgstr \"Language: yy\\n\"\nmsgid \"Cancel\"\nmsgstr \"Ycancel\"\n").expect("write");
        // Corrects one French string, keeps the rest.
        std::fs::write(dir.join("fr.po"), "\u{feff}msgid \"Cancel\"\r\nmsgstr \"Laisser tomber\"\r\n").expect("write");
        // Broken file: still listed, with its errors.
        std::fs::write(dir.join("zz.po"), [0xffu8, 0xfe, b'm', b's', b'g']).expect("write");
        std::fs::write(dir.join("notes.txt"), "ignored").expect("write");
        let reg = build(Some(&dir));
        let get = |code: &str| reg.langs.iter().find(|l| l.lang.code() == code).unwrap_or_else(|| panic!("{code} missing"));
        let xx = get("xx");
        assert_eq!(xx.name, "Xxish");
        assert!(!xx.embedded && xx.file.is_some() && xx.errors.is_empty());
        assert_eq!(xx.catalog.as_ref().and_then(|c| c.get("Cancel")), Some("Xcancel"));
        assert!(xx.completion > 0.0 && xx.completion < 0.01);
        assert_eq!(get("yy").name, "yy");
        let fr = get("fr");
        assert!(fr.embedded && fr.file.is_some());
        let cat = fr.catalog.as_ref().expect("fr");
        assert_eq!(cat.get("Cancel"), Some("Laisser tomber"));
        assert_eq!(cat.get("Language"), Some("Langue"));
        assert_eq!(fr.name, "Français");
        assert!(fr.completion > 0.99, "{}", fr.completion);
        assert!(!get("zz").errors.is_empty());
        assert_eq!(reg.langs[0].lang, Lang::EN);
        assert_eq!(reg.langs.len(), 1 + embedded().len() + 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_folder_is_fine() {
        let reg = build(Some(Path::new("/nonexistent/ponzik/lang")));
        assert!(reg.errors.is_empty());
        assert_eq!(reg.langs.len(), 1 + embedded().len());
        let fr = reg.langs.iter().find(|l| l.lang.code() == "fr").expect("fr");
        assert!((fr.completion - 1.0).abs() < 1e-6);
    }
}
