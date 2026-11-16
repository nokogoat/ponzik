//! Lecture des catalogues au format gettext `.po` (sous-ensemble) :
//!
//! ```text
//! msgid ""
//! msgstr ""
//! "Language-Name: Français\n"
//! "Plural-Forms: nplurals=2; plural=(n > 1);\n"
//! "X-Decimal-Separator: ,\n"
//!
//! #: gui/app.rs
//! msgctxt "verb"
//! msgid "Open"
//! msgstr "Ouvrir"
//! ```
//!
//! Les entrées vides (`msgstr ""`) sont ignorées : le texte anglais s'affiche.

use std::collections::HashMap;

pub struct Catalog {
    map: HashMap<&'static str, &'static str>,
    order: Vec<(&'static str, &'static str)>,
    /// Nom de la langue dans cette langue (en-tête `Language-Name`).
    pub name: Option<&'static str>,
    pub decimal: char,
    /// Règle du pluriel « n > 1 » (français) plutôt que « n != 1 ».
    pub zero_is_singular: bool,
    /// Lignes illisibles et doublons, signalés par les tests.
    pub errors: Vec<String>,
}

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

fn unquote(line: &str) -> Option<String> {
    let inner = line.trim().strip_prefix('"')?.strip_suffix('"')?;
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next()? {
            'n' => out.push('\n'),
            't' => out.push('\t'),
            '"' => out.push('"'),
            '\\' => out.push('\\'),
            _ => return None,
        }
    }
    Some(out)
}

#[derive(Clone, Copy, PartialEq)]
enum Field {
    None,
    Ctxt,
    Id,
    Str,
}

impl Catalog {
    pub fn parse(src: &str) -> Catalog {
        let mut cat = Catalog { map: HashMap::new(), order: Vec::new(), name: None, decimal: '.', zero_is_singular: false, errors: Vec::new() };
        let (mut ctxt, mut id, mut tx): (Option<String>, Option<String>, Option<String>) = (None, None, None);
        let mut field = Field::None;
        for (n, raw) in src.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (kw, rest) = match line.split_once(' ') {
                Some((kw, rest)) if !line.starts_with('"') => (kw, rest),
                _ => ("", line),
            };
            let Some(text) = unquote(rest) else {
                cat.errors.push(format!("line {}: {raw}", n + 1));
                continue;
            };
            let next = match kw {
                "msgctxt" => Field::Ctxt,
                "msgid" => Field::Id,
                "msgstr" => Field::Str,
                "" => field,
                _ => {
                    cat.errors.push(format!("line {}: {raw}", n + 1));
                    continue;
                }
            };
            if kw == "msgctxt" || (kw == "msgid" && field == Field::Str) {
                Self::flush(&mut cat, &mut ctxt, &mut id, &mut tx);
            }
            let slot = match next {
                Field::Ctxt => &mut ctxt,
                Field::Id => &mut id,
                Field::Str => &mut tx,
                Field::None => {
                    cat.errors.push(format!("line {}: {raw}", n + 1));
                    continue;
                }
            };
            if kw.is_empty() {
                slot.get_or_insert_default().push_str(&text);
            } else {
                *slot = Some(text);
            }
            field = next;
        }
        Self::flush(&mut cat, &mut ctxt, &mut id, &mut tx);
        cat
    }

    fn flush(cat: &mut Catalog, ctxt: &mut Option<String>, id: &mut Option<String>, tx: &mut Option<String>) {
        let (c, i, t) = (ctxt.take(), id.take(), tx.take());
        let (Some(i), Some(t)) = (i, t) else { return };
        if i.is_empty() {
            cat.header(&t);
            return;
        }
        let key = match c {
            Some(c) => format!("{c}\u{4}{i}"),
            None => i,
        };
        if t.is_empty() {
            return;
        }
        if cat.map.contains_key(key.as_str()) {
            cat.errors.push(format!("duplicate msgid {key:?}"));
            return;
        }
        let (k, v) = (leak(key), leak(t));
        cat.map.insert(k, v);
        cat.order.push((k, v));
    }

    fn header(&mut self, text: &str) {
        for line in text.lines() {
            let Some((k, v)) = line.split_once(':') else { continue };
            let v = v.trim();
            match k.trim() {
                "Language-Name" => self.name = Some(leak(v.to_string())),
                "X-Decimal-Separator" => self.decimal = v.chars().next().unwrap_or('.'),
                "Plural-Forms" => self.zero_is_singular = v.replace(' ', "").contains("n>1"),
                _ => {}
            }
        }
    }

    pub fn get(&self, key: &str) -> Option<&'static str> {
        self.map.get(key).copied()
    }

    pub fn contains(&self, key: &str) -> bool {
        self.map.contains_key(key)
    }

    /// Entrées dans l'ordre du fichier (clé anglaise, traduction).
    pub fn entries(&self) -> impl Iterator<Item = (&'static str, &'static str)> + '_ {
        self.order.iter().copied()
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_header_entries_context_and_continuations() {
        let src = r#"
msgid ""
msgstr ""
"Language-Name: Test\n"
"Plural-Forms: nplurals=2; plural=(n > 1);\n"
"X-Decimal-Separator: ,\n"

# comment
msgid "Open"
msgstr "Ouvrir"

msgctxt "state"
msgid "Open"
msgstr "Ouvert"

msgid "Two "
"lines"
msgstr "Deux \"lignes\"\n"

msgid "Untranslated"
msgstr ""
"#;
        let c = Catalog::parse(src);
        assert!(c.errors.is_empty(), "{:?}", c.errors);
        assert_eq!(c.name, Some("Test"));
        assert_eq!(c.decimal, ',');
        assert!(c.zero_is_singular);
        assert_eq!(c.get("Open"), Some("Ouvrir"));
        assert_eq!(c.get("state\u{4}Open"), Some("Ouvert"));
        assert_eq!(c.get("Two lines"), Some("Deux \"lignes\"\n"));
        assert_eq!(c.get("Untranslated"), None);
        assert_eq!(c.len(), 3);
    }

    #[test]
    fn reports_duplicates_and_garbage() {
        let c = Catalog::parse("msgid \"A\"\nmsgstr \"B\"\nmsgid \"A\"\nmsgstr \"C\"\nnonsense\n");
        assert_eq!(c.errors.len(), 2);
        assert_eq!(c.get("A"), Some("B"));
    }
}
