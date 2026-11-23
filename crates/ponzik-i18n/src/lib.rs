//! Translation layer of Ponzik.
//!
//! The source language is English: every user-visible string is written in
//! English in the code and goes through [`tr`] (or [`tr_fmt!`], [`tr_n`]…).
//! Other languages are gettext-like catalogs keyed by the English text; a
//! missing entry falls back to English. Catalogs come from two places:
//!
//! - `lang/<code>.po` in this crate, embedded at compile time;
//! - the user's translation folder (`<config>/lang/*.po`, see
//!   [`set_user_dir`]), read at run time and reloadable with [`reload`]. A
//!   user file completes or corrects an embedded language of the same code.
//!
//! ```
//! use ponzik_i18n::{tr, tr_fmt};
//! let title = tr("Save project");
//! let msg = tr_fmt!("{} notes deleted", 3);
//! # let _ = (title, msg);
//! ```

mod catalog;
mod fmt;
pub mod known;
mod registry;
pub mod scan;
mod template;

use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

pub use catalog::{Catalog, Plural};
pub use fmt::{Arg, Spec, format, placeholders};
pub use registry::{LangInfo, Registry, normalize_code, reference_keys, registry, reload, set_user_dir, user_dir, valid_code};
pub use template::{Created, create_translation, missing_entries, parse_new_language, po_quote, template};

/// A language: English (built in) or a catalog, embedded or from the user's
/// folder. Identified by its code; stays valid across [`reload`]s.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Lang(pub(crate) usize);

impl Lang {
    pub const EN: Lang = Lang(0);

    /// Every available language, English first.
    pub fn all() -> impl Iterator<Item = Lang> {
        registry().langs.iter().map(|l| l.lang)
    }

    /// Language code (`en`, `fr`, `pt_BR`…), as saved in the settings.
    pub fn code(self) -> &'static str {
        registry::code_of(self)
    }

    /// Picker entry of the language (None if its file was removed).
    pub fn info(self) -> Option<&'static LangInfo> {
        registry().langs.iter().find(|l| l.lang == self)
    }

    /// Name of the language in that language (« English », « Français »).
    pub fn name(self) -> &'static str {
        self.info().map_or_else(|| self.code(), |l| l.name.as_str())
    }

    /// `fr`, `fr_FR.UTF-8`, `FR-ca`, `pt-br`… → the matching available
    /// language: same code, else the same language without region.
    pub fn from_code(code: &str) -> Option<Lang> {
        let code = normalize_code(code);
        if code.is_empty() {
            return None;
        }
        let base = code.split('_').next().unwrap_or("");
        let langs = &registry().langs;
        let base_of = |l: &LangInfo| l.lang.code().split('_').next() == Some(base);
        langs
            .iter()
            .find(|l| l.lang.code() == code)
            .or_else(|| langs.iter().find(|l| l.lang.code() == base))
            .or_else(|| langs.iter().find(|l| base_of(l)))
            .map(|l| l.lang)
    }

    /// Language of the system (`LC_ALL`, `LC_MESSAGES`, `LANG`), English when
    /// unknown or unavailable.
    pub fn detect() -> Lang {
        for var in ["LC_ALL", "LC_MESSAGES", "LANG"] {
            if let Ok(v) = std::env::var(var)
                && !v.is_empty()
            {
                return Lang::from_code(&v).unwrap_or(Lang::EN);
            }
        }
        Lang::EN
    }

    /// Translations (None for English).
    pub fn catalog(self) -> Option<&'static Catalog> {
        self.info().and_then(|l| l.catalog.as_ref())
    }

    /// Decimal separator of numbers (`.` in English, `,` in French).
    pub fn decimal_separator(self) -> char {
        self.catalog().map_or('.', |c| c.decimal)
    }

    /// Whether `n` takes the same form as 1 (the singular of [`tr_n`]).
    pub fn is_singular(self, n: u64) -> bool {
        match self.catalog() {
            Some(c) => c.plural.index(n) == c.plural.index(1),
            None => n == 1,
        }
    }
}

static CURRENT: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    static OVERRIDE: Cell<Option<Lang>> = const { Cell::new(None) };
}

/// Current language of the interface.
pub fn lang() -> Lang {
    OVERRIDE.with(Cell::get).unwrap_or(Lang(CURRENT.load(Ordering::Relaxed)))
}

/// Changes the language of the whole application, effective immediately.
pub fn set_lang(lang: Lang) {
    CURRENT.store(lang.0, Ordering::Relaxed);
}

/// Runs `f` with another language on this thread only (tests, previews).
pub fn with_lang<R>(lang: Lang, f: impl FnOnce() -> R) -> R {
    let was = OVERRIDE.with(|o| o.replace(Some(lang)));
    let r = f();
    OVERRIDE.with(|o| o.set(was));
    r
}

/// Localized text of an English source string; English when the current
/// catalog has no entry for it.
pub fn tr(text: &str) -> &str {
    match lang().catalog() {
        None => text,
        Some(c) => c.get(text).unwrap_or(text),
    }
}

/// Like [`tr`], with a context to tell apart two meanings of the same English
/// text (`msgctxt` in the catalog).
pub fn tr_ctx<'a>(context: &str, text: &'a str) -> &'a str {
    match lang().catalog() {
        None => text,
        Some(c) => c.get(&format!("{context}\u{4}{text}")).unwrap_or(text),
    }
}

/// Singular or plural form by the rules of the current language, translated.
pub fn tr_n<'a>(one: &'a str, other: &'a str, n: impl TryInto<u64>) -> &'a str {
    let n = n.try_into().unwrap_or(u64::MAX);
    tr(if lang().is_singular(n) { one } else { other })
}

/// Marks a string for the catalog without translating it now (tables in
/// `const` contexts, translated later with [`tr`] where they are shown).
pub const fn tr_noop(text: &'static str) -> &'static str {
    text
}

/// Decimal separator of the current language.
pub fn decimal_separator() -> char {
    lang().decimal_separator()
}

/// `value` with `decimals` decimals and the decimal separator of the language.
pub fn num(value: f64, decimals: usize) -> String {
    localize_number(format!("{value:.decimals$}"))
}

/// Replaces the decimal point of an already formatted number.
pub fn localize_number(s: String) -> String {
    match decimal_separator() {
        '.' => s,
        sep => s.replace('.', sep.encode_utf8(&mut [0; 4])),
    }
}

/// Reads a number typed by the user, accepting `.` and `,` as decimal mark.
pub fn parse_num(s: &str) -> Option<f64> {
    s.trim().replace(',', ".").replace('−', "-").parse().ok()
}

/// Translated text formatted with arguments: `{}`, `{0}`, `{:.2}`, `{:>5}`, `{:+}`.
/// Numbers follow the decimal separator of the language.
#[macro_export]
macro_rules! tr_fmt {
    ($text:literal $(, $arg:expr)* $(,)?) => {
        $crate::format($crate::tr($text), &[$(&$arg as &dyn $crate::Arg),*])
    };
}

/// Plural form chosen by `n` then formatted: `tr_fmt_n!("{} note", "{} notes", n, n)`.
#[macro_export]
macro_rules! tr_fmt_n {
    ($one:literal, $other:literal, $n:expr $(, $arg:expr)* $(,)?) => {
        $crate::format($crate::tr_n($one, $other, $n), &[$(&$arg as &dyn $crate::Arg),*])
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fr() -> Lang {
        Lang::from_code("fr_FR.UTF-8").expect("French catalog")
    }

    #[test]
    fn english_is_the_source() {
        with_lang(Lang::EN, || {
            assert_eq!(tr("Save"), "Save");
            assert_eq!(tr_fmt!("{} tracks", 3), "3 tracks");
            assert_eq!(tr_n("{} note", "{} notes", 0usize), "{} notes");
            assert_eq!(num(1.5, 2), "1.50");
        });
    }

    #[test]
    fn french_catalog_and_fallback() {
        let fr = fr();
        assert_eq!(fr.code(), "fr");
        assert_eq!(fr.name(), "Français");
        with_lang(fr, || {
            assert_eq!(tr("Cancel"), "Annuler");
            assert_eq!(tr("No such string, ever"), "No such string, ever");
            assert_eq!(tr_n("{} note", "{} notes", 1usize), tr("{} note"));
            assert!(lang().is_singular(0));
            assert_eq!(num(1.5, 2), "1,50");
            assert_eq!(parse_num(" 1,25 "), Some(1.25));
        });
    }

    #[test]
    fn languages_from_codes() {
        assert_eq!(Lang::from_code("en_US.UTF-8"), Some(Lang::EN));
        assert_eq!(Lang::from_code("C"), None);
        assert_eq!(Lang::from_code(""), None);
        assert!(Lang::all().any(|l| l.code() == "fr"));
    }

    #[test]
    fn every_catalog_is_well_formed() {
        for (code, cat) in registry::embedded() {
            assert!(cat.errors.is_empty(), "{code}: {:?}", cat.errors);
            assert!(cat.name.is_some(), "{code}: Language-Name header");
            for (en, tx) in cat.entries() {
                assert_eq!(placeholders(en), placeholders(tx), "{code}: placeholders of {en:?}");
            }
        }
    }

    #[test]
    fn user_folder_loads_and_reloads() {
        let dir = std::env::temp_dir().join(format!("ponzik-i18n-user-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let file = dir.join("qq.po");
        let po = |save: &str| {
            format!(
                "msgid \"\"\nmsgstr \"\"\n\"Language-Name: Qqish\\n\"\n\"Plural-Forms: nplurals=1; plural=0;\\n\"\n\"X-Decimal-Separator: ,\\n\"\n\nmsgid \"Save\"\nmsgstr \"{save}\"\n"
            )
        };
        std::fs::write(&file, po("Qsave")).expect("write");
        set_user_dir(Some(dir.clone()));
        let qq = Lang::from_code("qq_QQ.UTF-8").expect("user language");
        assert_eq!(qq.code(), "qq");
        assert_eq!(qq.name(), "Qqish");
        with_lang(qq, || {
            assert_eq!(tr("Save"), "Qsave");
            assert_eq!(tr("Cancel"), "Cancel", "untranslated: English");
            assert!(lang().is_singular(7));
            assert_eq!(num(0.5, 1), "0,5");
        });
        // Edited, then reloaded without restarting.
        std::fs::write(&file, po("Qsave2")).expect("write");
        reload();
        assert_eq!(Lang::from_code("qq"), Some(qq));
        with_lang(qq, || assert_eq!(tr("Save"), "Qsave2"));
        // Removed: the language disappears, its texts fall back to English.
        std::fs::remove_file(&file).expect("rm");
        reload();
        assert_eq!(Lang::from_code("qq"), None);
        with_lang(qq, || assert_eq!(tr("Save"), "Save"));
        assert_eq!(qq.name(), "qq");
        set_user_dir(None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
