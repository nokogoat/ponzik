//! "Create a translation": a ready-to-fill `.po` file with every string of
//! the application, the French translation as a hint for each one.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::registry::{embedded, normalize_code, reference_keys, valid_code};
use crate::{Catalog, known};

/// Quoted `.po` string; text with line breaks is split after each `\n`, as
/// gettext tools do.
pub fn po_quote(s: &str) -> String {
    let esc = |s: &str| {
        let mut out = String::with_capacity(s.len() + 2);
        out.push('"');
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\t' => out.push_str("\\t"),
                '\r' => out.push_str("\\r"),
                c => out.push(c),
            }
        }
        out.push('"');
        out
    };
    let body = s.strip_suffix('\n').unwrap_or(s);
    if !body.contains('\n') {
        return esc(s);
    }
    let mut out = String::from("\"\"");
    for line in s.split_inclusive('\n') {
        out.push('\n');
        out.push_str(&esc(line));
    }
    out
}

/// What the user typed in "Create a translation": `es`, `es Español`,
/// `pt-BR`, `Spanish`, `Español` → (code, name). None if no code can be found.
pub fn parse_new_language(input: &str) -> Option<(String, String)> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    if let Some(k) = known::by_name(input) {
        return Some((k.code.to_string(), k.native.to_string()));
    }
    let (first, rest) = input.split_once(char::is_whitespace).unwrap_or((input, ""));
    let code = normalize_code(first);
    if !valid_code(&code) {
        return None;
    }
    let name = rest.trim();
    let name = if name.is_empty() {
        known::find(&code).filter(|k| k.code == code).map_or_else(|| code.clone(), |k| k.native.to_string())
    } else {
        name.to_string()
    };
    Some((code, name))
}

fn header(code: &str, name: &str) -> String {
    let k = known::find(code);
    let plural = k.map_or("nplurals=2; plural=(n != 1);", |k| k.plural);
    let decimal = k.map_or('.', |k| k.decimal);
    let mut out = String::new();
    let _ = writeln!(out, "# Ponzik translation: {name} ({code})");
    out.push_str(
        "#\n\
         # How to translate (full guide: docs/TRANSLATING.md in the Ponzik repository):\n\
         # - Write the translation of each msgid in the msgstr below it. Leave a\n\
         #   msgstr empty to keep the English text.\n\
         # - Keep the {} fields: Ponzik puts numbers and names there ({0}, {:.1}…).\n\
         # - Lines starting with \"#. fr:\" show the French translation as a hint.\n\
         # - msgctxt tells apart two meanings of the same English text.\n\
         # - Save as UTF-8, then Settings > Language > Reload translations in\n\
         #   Ponzik: no restart needed.\n\
         # - Language-Name is the name shown in the language picker.\n\
         #   X-Decimal-Separator is the decimal mark of numbers (. or ,).\n\
         #\n",
    );
    out.push_str("msgid \"\"\nmsgstr \"\"\n");
    let _ = writeln!(out, "\"Project-Id-Version: Ponzik {}\\n\"", env!("CARGO_PKG_VERSION"));
    let _ = writeln!(out, "\"Language: {code}\\n\"");
    let _ = writeln!(out, "\"Language-Name: {}\\n\"", name.replace(['"', '\\'], ""));
    out.push_str("\"MIME-Version: 1.0\\n\"\n\"Content-Type: text/plain; charset=UTF-8\\n\"\n\"Content-Transfer-Encoding: 8bit\\n\"\n");
    let _ = writeln!(out, "\"Plural-Forms: {plural}\\n\"");
    let _ = writeln!(out, "\"X-Decimal-Separator: {decimal}\\n\"");
    out
}

/// `.po` entries for `keys`: the French hint, and the existing translation of
/// `prefill` (when the user corrects a shipped language).
fn entries<'a>(keys: impl Iterator<Item = &'a str>, prefill: Option<&Catalog>) -> (String, usize) {
    let fr = embedded().iter().find(|(c, _)| *c == "fr").map(|(_, c)| c);
    let mut out = String::new();
    let mut n = 0;
    for key in keys {
        n += 1;
        out.push('\n');
        let (ctx, id) = match key.split_once('\u{4}') {
            Some((c, i)) => (Some(c), i),
            None => (None, key),
        };
        let current = prefill.and_then(|c| c.get(key));
        if current.is_none()
            && let Some(hint) = fr.and_then(|c| c.get(key))
        {
            let _ = writeln!(out, "#. fr: {}", hint.replace('\n', "\\n"));
        }
        if let Some(ctx) = ctx {
            let _ = writeln!(out, "msgctxt {}", po_quote(ctx));
        }
        let _ = writeln!(out, "msgid {}", po_quote(id));
        let _ = writeln!(out, "msgstr {}", po_quote(current.unwrap_or("")));
    }
    (out, n)
}

/// A new catalog to fill: header, then every string of the application.
pub fn template(code: &str, name: &str) -> String {
    let prefill = embedded().iter().find(|(c, _)| normalize_code(c) == code).map(|(_, c)| c);
    let (body, _) = entries(reference_keys().iter().map(String::as_str), prefill);
    header(code, name) + &body
}

/// Entries of the application missing from an existing catalog, to append to
/// it after an update of Ponzik: (text, count).
pub fn missing_entries(existing: &Catalog) -> (String, usize) {
    let have: std::collections::HashSet<&str> = existing.ids().collect();
    let (body, n) = entries(reference_keys().iter().map(String::as_str).filter(|k| !have.contains(k)), None);
    if n == 0 {
        return (String::new(), 0);
    }
    (format!("\n# --- New strings ({n}) ---\n{body}"), n)
}

/// Result of [`create_translation`].
#[derive(Debug)]
pub struct Created {
    pub path: PathBuf,
    pub code: String,
    pub name: String,
    /// The file was there already: only the missing strings were appended.
    pub existed: bool,
    /// Strings written (all of them for a new file).
    pub added: usize,
}

/// Writes `<dir>/<code>.po` for the language typed by the user; an existing
/// file is never overwritten, it only receives the strings it lacks.
pub fn create_translation(dir: &Path, input: &str) -> Result<Created, String> {
    let (code, name) = parse_new_language(input).ok_or_else(|| format!("no language code in {input:?}"))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join(format!("{code}.po"));
    if path.exists() {
        let text = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let existing = Catalog::parse(&String::from_utf8_lossy(&text));
        let (more, added) = missing_entries(&existing);
        if added > 0 {
            let mut text = text;
            if !text.ends_with(b"\n") {
                text.push(b'\n');
            }
            text.extend_from_slice(more.as_bytes());
            std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
        }
        return Ok(Created { path, code, name, existed: true, added });
    }
    let text = template(&code, &name);
    let added = reference_keys().len();
    std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Created { path, code, name, existed: false, added })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_round_trips() {
        for s in ["plain", "a \"quote\" and \\ back", "two\nlines\n", "tab\there", "\n", "", "end\n\nblank"] {
            let src = format!("msgid \"k\"\nmsgstr {}\n", po_quote(s));
            let c = Catalog::parse(&src);
            assert!(c.errors.is_empty(), "{s:?}: {:?}", c.errors);
            assert_eq!(c.get("k"), if s.is_empty() { None } else { Some(s) }, "{src}");
        }
    }

    #[test]
    fn new_language_from_what_the_user_types() {
        let p = |s: &str| parse_new_language(s);
        assert_eq!(p("es"), Some(("es".into(), "Español".into())));
        assert_eq!(p(" pt-br "), Some(("pt_BR".into(), "Português (Brasil)".into())));
        assert_eq!(p("es_MX Español de México"), Some(("es_MX".into(), "Español de México".into())));
        assert_eq!(p("es_MX"), Some(("es_MX".into(), "es_MX".into())));
        assert_eq!(p("Spanish"), Some(("es".into(), "Español".into())));
        assert_eq!(p("deutsch"), Some(("de".into(), "Deutsch".into())));
        assert_eq!(p("tlh Klingon"), Some(("tlh".into(), "Klingon".into())));
        assert_eq!(p("Klingonese"), None);
        assert_eq!(p(""), None);
        assert_eq!(p("x"), None);
    }

    #[test]
    fn template_parses_and_lists_every_key() {
        let text = template("es", "Español");
        let c = Catalog::parse(&text);
        assert!(c.errors.is_empty(), "{:?}", &c.errors[..c.errors.len().min(5)]);
        assert_eq!(c.language.as_deref(), Some("es"));
        assert_eq!(c.name.as_deref(), Some("Español"));
        assert_eq!(c.decimal, ',');
        assert!(c.is_empty(), "nothing translated yet");
        let ids: std::collections::HashSet<&str> = c.ids().collect();
        let keys = reference_keys();
        assert!(keys.len() > 500);
        for k in keys {
            assert!(ids.contains(k.as_str()), "template lacks {k:?}");
        }
        assert_eq!(ids.len(), keys.len());
        assert!(text.contains("#. fr: Annuler\nmsgid \"Cancel\"\nmsgstr \"\""));
        assert!(text.contains("msgctxt \"drum pad\""));
    }

    #[test]
    fn template_of_a_shipped_language_is_prefilled() {
        let c = Catalog::parse(&template("fr", "Français"));
        assert!(c.errors.is_empty(), "{:?}", &c.errors[..c.errors.len().min(5)]);
        assert_eq!(c.get("Cancel"), Some("Annuler"));
        assert_eq!(c.decimal, ',');
    }

    #[test]
    fn create_never_overwrites_and_appends_missing_strings() {
        let dir = std::env::temp_dir().join(format!("ponzik-i18n-create-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let made = create_translation(&dir, "es").expect("create");
        assert!(!made.existed && made.path.ends_with("es.po"));
        assert_eq!(made.added, reference_keys().len());
        // A translator's work, and a string that is not there any more.
        let mut text = std::fs::read_to_string(&made.path).expect("read");
        text = text.replacen("msgid \"Cancel\"\nmsgstr \"\"", "msgid \"Cancel\"\nmsgstr \"Cancelar\"", 1);
        let first = reference_keys().iter().find(|k| !k.contains('\u{4}') && *k != "Cancel").expect("key");
        text = text.replacen(&format!("msgid {}\nmsgstr \"\"\n", po_quote(first)), "", 1);
        std::fs::write(&made.path, &text).expect("write");
        let again = create_translation(&dir, "es Español").expect("update");
        assert!(again.existed);
        assert_eq!(again.added, 1);
        let c = Catalog::parse(&std::fs::read_to_string(&made.path).expect("read"));
        assert!(c.errors.is_empty(), "{:?}", c.errors);
        assert_eq!(c.get("Cancel"), Some("Cancelar"));
        assert_eq!(c.ids().count(), reference_keys().len());
        assert!(create_translation(&dir, "???").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
