//! Common languages, so that "Create a translation" can fill the header of a
//! new catalog (name, plural rule, decimal separator) from a code or a name.

const ONE: &str = "nplurals=2; plural=(n != 1);";
const ZERO_ONE: &str = "nplurals=2; plural=(n > 1);";
const NONE: &str = "nplurals=1; plural=0;";
const SLAVIC: &str = "nplurals=3; plural=(n%10==1 && n%100!=11 ? 0 : n%10>=2 && n%10<=4 && (n%100<10 || n%100>=20) ? 1 : 2);";

pub struct Known {
    pub code: &'static str,
    pub english: &'static str,
    pub native: &'static str,
    pub plural: &'static str,
    pub decimal: char,
}

const fn k(code: &'static str, english: &'static str, native: &'static str, plural: &'static str, decimal: char) -> Known {
    Known { code, english, native, plural, decimal }
}

pub const LANGUAGES: &[Known] = &[
    k("ar", "Arabic", "العربية", "nplurals=6; plural=(n==0 ? 0 : n==1 ? 1 : n==2 ? 2 : n%100>=3 && n%100<=10 ? 3 : n%100>=11 ? 4 : 5);", '.'),
    k("bg", "Bulgarian", "Български", ONE, ','),
    k("ca", "Catalan", "Català", ONE, ','),
    k("cs", "Czech", "Čeština", "nplurals=3; plural=(n==1) ? 0 : (n>=2 && n<=4) ? 1 : 2;", ','),
    k("da", "Danish", "Dansk", ONE, ','),
    k("de", "German", "Deutsch", ONE, ','),
    k("el", "Greek", "Ελληνικά", ONE, ','),
    k("en", "English", "English", ONE, '.'),
    k("es", "Spanish", "Español", ONE, ','),
    k("et", "Estonian", "Eesti", ONE, ','),
    k("eu", "Basque", "Euskara", ONE, ','),
    k("fa", "Persian", "فارسی", ZERO_ONE, '.'),
    k("fi", "Finnish", "Suomi", ONE, ','),
    k("fr", "French", "Français", ZERO_ONE, ','),
    k("gl", "Galician", "Galego", ONE, ','),
    k("he", "Hebrew", "עברית", ONE, '.'),
    k("hi", "Hindi", "हिन्दी", ONE, '.'),
    k("hr", "Croatian", "Hrvatski", SLAVIC, ','),
    k("hu", "Hungarian", "Magyar", ONE, ','),
    k("id", "Indonesian", "Bahasa Indonesia", NONE, ','),
    k("it", "Italian", "Italiano", ONE, ','),
    k("ja", "Japanese", "日本語", NONE, '.'),
    k("ko", "Korean", "한국어", NONE, '.'),
    k("lt", "Lithuanian", "Lietuvių", SLAVIC, ','),
    k("nb", "Norwegian", "Norsk bokmål", ONE, ','),
    k("nl", "Dutch", "Nederlands", ONE, ','),
    k("pl", "Polish", "Polski", "nplurals=3; plural=(n==1 ? 0 : n%10>=2 && n%10<=4 && (n%100<10 || n%100>=20) ? 1 : 2);", ','),
    k("pt", "Portuguese", "Português", ONE, ','),
    k("pt_BR", "Brazilian Portuguese", "Português (Brasil)", ZERO_ONE, ','),
    k("ro", "Romanian", "Română", "nplurals=3; plural=(n==1 ? 0 : (n==0 || (n%100 > 0 && n%100 < 20)) ? 1 : 2);", ','),
    k("ru", "Russian", "Русский", SLAVIC, ','),
    k("sk", "Slovak", "Slovenčina", "nplurals=3; plural=(n==1) ? 0 : (n>=2 && n<=4) ? 1 : 2;", ','),
    k("sl", "Slovenian", "Slovenščina", "nplurals=4; plural=(n%100==1 ? 0 : n%100==2 ? 1 : n%100==3 || n%100==4 ? 2 : 3);", ','),
    k("sr", "Serbian", "Српски", SLAVIC, ','),
    k("sv", "Swedish", "Svenska", ONE, ','),
    k("th", "Thai", "ไทย", NONE, '.'),
    k("tr", "Turkish", "Türkçe", ZERO_ONE, ','),
    k("uk", "Ukrainian", "Українська", SLAVIC, ','),
    k("vi", "Vietnamese", "Tiếng Việt", NONE, ','),
    k("zh_CN", "Chinese (Simplified)", "简体中文", NONE, '.'),
    k("zh_TW", "Chinese (Traditional)", "繁體中文", NONE, '.'),
];

/// Exact code first, then the language without its region (`es_MX` → `es`).
pub fn find(code: &str) -> Option<&'static Known> {
    LANGUAGES.iter().find(|l| l.code == code).or_else(|| {
        let base = code.split('_').next().unwrap_or(code);
        LANGUAGES.iter().find(|l| l.code == base)
    })
}

/// Name of a language in that language, when Ponzik knows it.
pub fn native_name(code: &str) -> Option<&'static str> {
    LANGUAGES.iter().find(|l| l.code == code).map(|l| l.native)
}

/// A language by its English or native name, any case.
pub fn by_name(name: &str) -> Option<&'static Known> {
    let name = name.trim().to_lowercase();
    LANGUAGES.iter().find(|l| l.english.to_lowercase() == name || l.native.to_lowercase() == name)
}
