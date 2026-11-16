//! Extraction des textes à traduire depuis le code source : les tests
//! vérifient ainsi que chaque `tr("…")` a son entrée dans chaque catalogue.
//!
//! Reconnaît `tr`, `tr_n`, `tr_ctx`, `tr_noop`, `tr_fmt!` et `tr_fmt_n!` suivis
//! de littéraux de chaîne ; ignore les commentaires et le contenu des chaînes.

use std::path::Path;

const FUNCS: [&str; 6] = ["tr_fmt_n", "tr_fmt", "tr_noop", "tr_ctx", "tr_n", "tr"];

/// Lit un littéral de chaîne Rust à `i` (`"…"`, `r"…"`, `r#"…"#`), renvoie
/// (contenu, position après le littéral).
fn literal(src: &str, mut i: usize) -> Option<(String, usize)> {
    let b = src.as_bytes();
    if b.get(i) == Some(&b'r') {
        i += 1;
        let hashes = b[i..].iter().take_while(|&&c| c == b'#').count();
        i += hashes;
        if b.get(i) != Some(&b'"') {
            return None;
        }
        let close = format!("\"{}", "#".repeat(hashes));
        let end = src[i + 1..].find(&close)? + i + 1;
        return Some((src[i + 1..end].to_string(), end + close.len()));
    }
    if b.get(i) != Some(&b'"') {
        return None;
    }
    let mut out = String::new();
    let mut chars = src[i + 1..].char_indices();
    while let Some((k, c)) = chars.next() {
        match c {
            '"' => return Some((out, i + 1 + k + 1)),
            '\\' => match chars.next()?.1 {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                '0' => out.push('\0'),
                '\\' => out.push('\\'),
                '"' => out.push('"'),
                '\'' => out.push('\''),
                'u' => {
                    let hex: String = chars.by_ref().map(|(_, c)| c).skip(1).take_while(|&c| c != '}').collect();
                    out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                }
                '\n' => {
                    while let Some((_, c)) = chars.clone().next() {
                        if !c.is_whitespace() {
                            break;
                        }
                        chars.next();
                    }
                }
                _ => return None,
            },
            c => out.push(c),
        }
    }
    None
}

fn skip_ws(src: &str, mut i: usize) -> usize {
    let b = src.as_bytes();
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

/// Fin d'un littéral ou commentaire commençant à `i`, s'il y en a un.
fn skip_non_code(src: &str, i: usize) -> Option<usize> {
    let b = src.as_bytes();
    match b[i] {
        b'/' if b.get(i + 1) == Some(&b'/') => Some(src[i..].find('\n').map_or(src.len(), |e| i + e)),
        b'/' if b.get(i + 1) == Some(&b'*') => Some(src[i + 2..].find("*/").map_or(src.len(), |e| i + 2 + e + 2)),
        b'"' => literal(src, i).map(|(_, e)| e).or(Some(src.len())),
        b'r' if (b.get(i + 1) == Some(&b'"') || b.get(i + 1) == Some(&b'#')) && (i == 0 || !is_ident(b[i - 1])) => {
            literal(src, i).map(|(_, e)| e)
        }
        b'\'' => {
            // Littéral de caractère ('x', '\n', '\u{..}') ; sinon durée de vie.
            if b.get(i + 1) == Some(&b'\\') {
                src[i + 2..].find('\'').map(|e| i + 2 + e + 1)
            } else {
                let c = src[i + 1..].chars().next()?;
                let after = i + 1 + c.len_utf8();
                (b.get(after) == Some(&b'\'')).then_some(after + 1)
            }
        }
        _ => None,
    }
}

fn is_ident(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// Clés de catalogue trouvées dans un fichier source (contexte inclus sous la
/// forme `contexte\u{4}texte`).
pub fn keys_in_source(src: &str) -> Vec<String> {
    let b = src.as_bytes();
    let mut keys = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if let Some(end) = skip_non_code(src, i) {
            i = end.max(i + 1);
            continue;
        }
        if !b[i].is_ascii_alphabetic() || (i > 0 && is_ident(b[i - 1])) {
            i += 1;
            continue;
        }
        let word_end = i + b[i..].iter().take_while(|&&c| is_ident(c)).count();
        let word = &src[i..word_end];
        let Some(func) = FUNCS.iter().find(|f| **f == word) else {
            i = word_end;
            continue;
        };
        let mut j = word_end;
        if b.get(j) == Some(&b'!') {
            j += 1;
        }
        if b.get(j) != Some(&b'(') {
            i = word_end;
            continue;
        }
        j = skip_ws(src, j + 1);
        let Some((first, after)) = literal(src, j) else {
            i = word_end;
            continue;
        };
        let second = || {
            let k = skip_ws(src, after);
            (b.get(k) == Some(&b',')).then(|| literal(src, skip_ws(src, k + 1))).flatten()
        };
        match *func {
            "tr_ctx" => {
                if let Some((text, _)) = second() {
                    keys.push(format!("{first}\u{4}{text}"));
                }
            }
            "tr_n" | "tr_fmt_n" => {
                keys.push(first);
                if let Some((other, _)) = second() {
                    keys.push(other);
                }
            }
            _ => keys.push(first),
        }
        i = after;
    }
    keys
}

/// Clés de tous les fichiers `.rs` sous `dir` (récursif), avec leur fichier.
pub fn keys_in_dir(dir: &Path) -> Vec<(String, std::path::PathBuf)> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else { return out };
    let mut paths: Vec<_> = rd.filter_map(Result::ok).map(|e| e.path()).collect();
    paths.sort();
    for p in paths {
        if p.is_dir() {
            out.extend(keys_in_dir(&p));
        } else if p.extension().is_some_and(|x| x == "rs")
            && let Ok(src) = std::fs::read_to_string(&p)
        {
            out.extend(keys_in_source(&src).into_iter().map(|k| (k, p.clone())));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_calls_and_skips_comments_and_strings() {
        let src = r##"
            // tr("in a comment")
            /* tr("in a block") */
            let a = tr("Save");
            let b = ponzik_i18n::tr( "Open \"file\"\n" );
            let c = "tr(\"inside a string\")";
            let d = tr_fmt!("{} tracks", n);
            let e = tr_n("{} note", "{} notes", n);
            let f = tr_ctx("verb", "Record");
            let g = tr_noop(r#"Raw "one""#);
            let h = self.tr(x);
            let i = attr("no");
            let j = '"'; let k = tr("After char");
            fn life<'a>(x: &'a str) { tr("Line \
                     joined") }
        "##;
        assert_eq!(
            keys_in_source(src),
            [
                "Save",
                "Open \"file\"\n",
                "{} tracks",
                "{} note",
                "{} notes",
                "verb\u{4}Record",
                "Raw \"one\"",
                "After char",
                "Line joined"
            ]
        );
    }
}
