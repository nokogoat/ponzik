//! JSON minimal et sans dépendance : analyse avec erreurs localisées (ligne:colonne),
//! écriture indentée. Les objets gardent l'ordre de leurs clés.

use std::fmt::Write as _;

use ponzik_i18n::{tr, tr_fmt};

#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    /// Paires dans l'ordre du fichier.
    Obj(Vec<(String, Json)>),
}

/// Profondeur d'imbrication maximale acceptée par l'analyseur.
const MAX_DEPTH: usize = 256;

impl Json {
    /// Valeur d'une clé (la première si elle est répétée).
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match *self {
            Json::Num(n) => Some(n),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match *self {
            Json::Bool(b) => Some(b),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_arr(&self) -> Option<&[Json]> {
        match self {
            Json::Arr(a) => Some(a),
            _ => None,
        }
    }

    pub fn as_obj(&self) -> Option<&[(String, Json)]> {
        match self {
            Json::Obj(o) => Some(o),
            _ => None,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Json::Null)
    }

    /// Texte indenté (2 espaces), terminé par un saut de ligne.
    pub fn pretty(&self) -> String {
        let mut out = String::new();
        write_value(&mut out, self, 0);
        out.push('\n');
        out
    }

    /// Texte compact, sur une ligne.
    pub fn compact(&self) -> String {
        let mut out = String::new();
        write_compact(&mut out, self);
        out
    }
}

impl From<bool> for Json {
    fn from(v: bool) -> Self {
        Json::Bool(v)
    }
}

impl From<f64> for Json {
    fn from(v: f64) -> Self {
        Json::Num(v)
    }
}

impl From<f32> for Json {
    fn from(v: f32) -> Self {
        Json::Num(v as f64)
    }
}

macro_rules! from_int {
    ($($t:ty),*) => {$(
        impl From<$t> for Json {
            fn from(v: $t) -> Self {
                Json::Num(v as f64)
            }
        }
    )*};
}
from_int!(u8, u16, u32, i32, i64, usize);

impl From<&str> for Json {
    fn from(v: &str) -> Self {
        Json::Str(v.into())
    }
}

impl From<String> for Json {
    fn from(v: String) -> Self {
        Json::Str(v)
    }
}

impl<T: Into<Json>> From<Option<T>> for Json {
    fn from(v: Option<T>) -> Self {
        v.map_or(Json::Null, Into::into)
    }
}

impl<T: Into<Json>> From<Vec<T>> for Json {
    fn from(v: Vec<T>) -> Self {
        Json::Arr(v.into_iter().map(Into::into).collect())
    }
}

// --- Écriture ---

/// Nombre au format JSON : entier sans virgule, `f32` au plus court qui relit
/// exactement la même valeur, sinon `f64` au plus court ; non fini → `null`.
pub fn format_number(n: f64) -> String {
    if !n.is_finite() {
        return "null".into();
    }
    if n.fract() == 0.0 && n.abs() < 9.007_199_254_740_992e15 {
        // `-0` reste un entier : on écrit `0`.
        return format!("{}", n as i64);
    }
    let small_or_big = n.abs() < 1e-5 || n.abs() >= 1e21;
    let f = n as f32;
    if f as f64 == n {
        if small_or_big { format!("{f:e}") } else { format!("{f}") }
    } else if small_or_big {
        format!("{n:e}")
    } else {
        format!("{n}")
    }
}

fn write_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || c == '\u{2028}' || c == '\u{2029}' => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn indent(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push_str("  ");
    }
}

/// Tableau sans conteneur imbriqué (notes, paramètres) : écrit sur une ligne.
fn is_flat(items: &[Json]) -> bool {
    items.iter().all(|v| !matches!(v, Json::Arr(_) | Json::Obj(_)))
}

fn write_value(out: &mut String, v: &Json, depth: usize) {
    match v {
        Json::Arr(items) if items.is_empty() => out.push_str("[]"),
        Json::Obj(pairs) if pairs.is_empty() => out.push_str("{}"),
        Json::Arr(items) if is_flat(items) => write_compact(out, v),
        Json::Arr(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push('\n');
                indent(out, depth + 1);
                write_value(out, item, depth + 1);
            }
            out.push('\n');
            indent(out, depth);
            out.push(']');
        }
        Json::Obj(pairs) => {
            out.push('{');
            for (i, (k, item)) in pairs.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push('\n');
                indent(out, depth + 1);
                write_string(out, k);
                out.push_str(": ");
                write_value(out, item, depth + 1);
            }
            out.push('\n');
            indent(out, depth);
            out.push('}');
        }
        _ => write_compact(out, v),
    }
}

fn write_compact(out: &mut String, v: &Json) {
    match v {
        Json::Null => out.push_str("null"),
        Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Json::Num(n) => out.push_str(&format_number(*n)),
        Json::Str(s) => write_string(out, s),
        Json::Arr(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_compact(out, item);
            }
            out.push(']');
        }
        Json::Obj(pairs) => {
            out.push('{');
            for (i, (k, item)) in pairs.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_string(out, k);
                out.push_str(": ");
                write_compact(out, item);
            }
            out.push('}');
        }
    }
}

// --- Analyse ---

/// Analyse un document JSON complet. Erreur : « ligne L, colonne C : message ».
pub fn parse(text: &str) -> Result<Json, String> {
    let mut p = Parser { src: text.as_bytes(), pos: 0 };
    // Une éventuelle marque d'ordre des octets est ignorée.
    if p.src.starts_with(&[0xEF, 0xBB, 0xBF]) {
        p.pos = 3;
    }
    let v = p.value(0)?;
    p.skip_ws();
    if p.pos < p.src.len() {
        return Err(p.error(tr("unexpected content after the end of the document")));
    }
    Ok(v)
}

struct Parser<'a> {
    src: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn error(&self, msg: &str) -> String {
        self.error_at(self.pos, msg)
    }

    fn error_at(&self, at: usize, msg: &str) -> String {
        let at = at.min(self.src.len());
        let before = &self.src[..at];
        let line = before.iter().filter(|&&b| b == b'\n').count() + 1;
        let line_start = before.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
        // Colonne en caractères, pas en octets.
        let col = String::from_utf8_lossy(&before[line_start..]).chars().count() + 1;
        tr_fmt!("line {}, column {}: {}", line, col, msg)
    }

    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while let Some(b' ' | b'\t' | b'\n' | b'\r') = self.peek() {
            self.pos += 1;
        }
    }

    fn expect(&mut self, b: u8) -> Result<(), String> {
        if self.peek() == Some(b) {
            self.pos += 1;
            Ok(())
        } else {
            Err(self.unexpected(&tr_fmt!("expected \"{}\"", b as char)))
        }
    }

    fn unexpected(&self, msg: &str) -> String {
        match self.peek() {
            None => self.error(&tr_fmt!("unexpected end of text, {}", msg)),
            Some(_) => {
                let c = std::str::from_utf8(&self.src[self.pos..])
                    .ok()
                    .and_then(|s| s.chars().next())
                    .unwrap_or('\u{FFFD}');
                self.error(&tr_fmt!("unexpected character \"{}\", {}", c.escape_debug().to_string(), msg))
            }
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, String> {
        if depth > MAX_DEPTH {
            return Err(self.error(tr("nesting too deep")));
        }
        self.skip_ws();
        match self.peek() {
            Some(b'{') => self.object(depth),
            Some(b'[') => self.array(depth),
            Some(b'"') => self.string().map(Json::Str),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(b't') => self.keyword("true", Json::Bool(true)),
            Some(b'f') => self.keyword("false", Json::Bool(false)),
            Some(b'n') => self.keyword("null", Json::Null),
            _ => Err(self.unexpected(tr("expected a value"))),
        }
    }

    fn keyword(&mut self, word: &str, v: Json) -> Result<Json, String> {
        if self.src[self.pos..].starts_with(word.as_bytes()) {
            self.pos += word.len();
            Ok(v)
        } else {
            Err(self.unexpected(tr("expected a value")))
        }
    }

    fn object(&mut self, depth: usize) -> Result<Json, String> {
        self.pos += 1;
        let mut pairs = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Json::Obj(pairs));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some(b'"') {
                return Err(self.unexpected(tr("expected a quoted key")));
            }
            let key = self.string()?;
            self.skip_ws();
            self.expect(b':')?;
            let v = self.value(depth + 1)?;
            pairs.push((key, v));
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(Json::Obj(pairs));
                }
                _ => return Err(self.unexpected(tr("expected \",\" or \"}\""))),
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<Json, String> {
        self.pos += 1;
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Json::Arr(items));
        }
        loop {
            items.push(self.value(depth + 1)?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Json::Arr(items));
                }
                _ => return Err(self.unexpected(tr("expected \",\" or \"]\""))),
            }
        }
    }

    fn number(&mut self) -> Result<Json, String> {
        let start = self.pos;
        let digits = |p: &mut Self| {
            let s = p.pos;
            while let Some(b'0'..=b'9') = p.peek() {
                p.pos += 1;
            }
            p.pos - s
        };
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        let int_start = self.pos;
        match digits(self) {
            0 => return Err(self.unexpected(tr("expected a digit"))),
            n if n > 1 && self.src[int_start] == b'0' => {
                return Err(self.error_at(int_start, tr("leading zero not allowed in a number")));
            }
            _ => {}
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            if digits(self) == 0 {
                return Err(self.unexpected(tr("expected a digit after the decimal point")));
            }
        }
        if let Some(b'e' | b'E') = self.peek() {
            self.pos += 1;
            if let Some(b'+' | b'-') = self.peek() {
                self.pos += 1;
            }
            if digits(self) == 0 {
                return Err(self.unexpected(tr("expected a digit in the exponent")));
            }
        }
        // Que de l'ASCII : la conversion ne peut pas échouer.
        let text = std::str::from_utf8(&self.src[start..self.pos]).unwrap_or("0");
        text.parse::<f64>().map(Json::Num).map_err(|_| self.error_at(start, tr("invalid number")))
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let at = self.pos;
        let hex = self.src.get(self.pos..self.pos + 4).ok_or_else(|| self.error(tr("incomplete \\u sequence")))?;
        let text = std::str::from_utf8(hex).map_err(|_| self.error_at(at, tr("invalid \\u sequence")))?;
        let v = u32::from_str_radix(text, 16).map_err(|_| self.error_at(at, tr("invalid \\u sequence")))?;
        if !text.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(self.error_at(at, tr("invalid \\u sequence")));
        }
        self.pos += 4;
        Ok(v)
    }

    fn string(&mut self) -> Result<String, String> {
        let open = self.pos;
        self.pos += 1;
        let mut out = String::new();
        loop {
            // Avance jusqu'au prochain caractère spécial, en recopiant d'un bloc.
            let run = self.pos;
            while let Some(b) = self.peek() {
                if b == b'"' || b == b'\\' || b < 0x20 {
                    break;
                }
                self.pos += 1;
            }
            // `src` vient d'un `&str` et les coupures tombent sur de l'ASCII : UTF-8 valide.
            out.push_str(std::str::from_utf8(&self.src[run..self.pos]).unwrap_or("\u{FFFD}"));
            match self.peek() {
                None => return Err(self.error_at(open, tr("unterminated string"))),
                Some(b'"') => {
                    self.pos += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.pos += 1;
                    let esc = self.peek().ok_or_else(|| self.error_at(open, tr("unterminated string")))?;
                    self.pos += 1;
                    match esc {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{08}'),
                        b'f' => out.push('\u{0C}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let at = self.pos - 2;
                            let hi = self.hex4()?;
                            let code = match hi {
                                0xD800..=0xDBFF => {
                                    if !self.src[self.pos..].starts_with(b"\\u") {
                                        return Err(self.error_at(at, tr("UTF-16 high surrogate without its low surrogate")));
                                    }
                                    self.pos += 2;
                                    let lo = self.hex4()?;
                                    if !(0xDC00..=0xDFFF).contains(&lo) {
                                        return Err(self.error_at(at, tr("invalid UTF-16 surrogate pair")));
                                    }
                                    0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                                }
                                0xDC00..=0xDFFF => return Err(self.error_at(at, tr("lone UTF-16 low surrogate"))),
                                c => c,
                            };
                            out.push(char::from_u32(code).ok_or_else(|| self.error_at(at, tr("invalid code point")))?);
                        }
                        _ => return Err(self.error_at(self.pos - 2, tr("unknown escape"))),
                    }
                }
                Some(_) => return Err(self.error(tr("control character not allowed in a string"))),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_values_and_keeps_key_order() {
        let v = parse(r#" { "b": [1, -2.5, 3e2, true, false, null], "a": {"x": "y"} } "#).unwrap();
        let Json::Obj(pairs) = &v else { panic!() };
        assert_eq!(pairs[0].0, "b");
        assert_eq!(pairs[1].0, "a");
        assert_eq!(
            v.get("b").unwrap().as_arr().unwrap(),
            [Json::Num(1.0), Json::Num(-2.5), Json::Num(300.0), Json::Bool(true), Json::Bool(false), Json::Null]
        );
        assert_eq!(v.get("a").and_then(|a| a.get("x")).and_then(Json::as_str), Some("y"));
    }

    #[test]
    fn string_escapes_and_surrogates() {
        let v = parse(r#""a\"b\\c\/d\b\f\n\r\té€🎵""#).unwrap();
        assert_eq!(v.as_str(), Some("a\"b\\c/d\u{08}\u{0C}\n\r\té€🎵"));
        assert!(parse(r#""\ud83c""#).unwrap_err().contains("without its low surrogate"));
        assert!(parse(r#""\udfb5""#).unwrap_err().contains("lone"));
        assert!(parse(r#""\ud83cA""#).unwrap_err().contains("surrogate"));
        assert!(parse(r#""\uZZZZ""#).is_err());
        assert!(parse(r#""\q""#).unwrap_err().contains("escape"));
        assert!(parse("\"a\nb\"").unwrap_err().contains("control character"));
        // Écriture puis relecture : identique.
        let s = "guillemet \" barre \\ ligne\n tab\t ctrl\u{01} é 🎵 \u{2028}";
        let text = Json::Str(s.into()).pretty();
        assert_eq!(parse(&text).unwrap().as_str(), Some(s));
    }

    #[test]
    fn errors_have_line_and_column() {
        let e = parse("{\n  \"a\": 1,\n  \"b\" 2\n}").unwrap_err();
        assert!(e.starts_with("line 3, column 7"), "{e}");
        let e = parse("[1, 2").unwrap_err();
        assert!(e.contains("end of text"), "{e}");
        let e = parse("\"é\" x").unwrap_err();
        assert!(e.starts_with("line 1, column 5"), "{e}");
        assert!(parse("01").unwrap_err().contains("leading zero"));
        assert!(parse("1.").is_err());
        assert!(parse("-").is_err());
        assert!(parse("1e").is_err());
        assert!(parse("[1,]").is_err());
        assert!(parse("{\"a\":1,}").is_err());
        assert!(parse("tru").is_err());
        assert!(parse("").is_err());
        assert!(parse(&"[".repeat(1_000)).unwrap_err().contains("too deep"));
        assert!(parse("\u{FEFF}[1]").is_ok());
    }

    #[test]
    fn numbers_round_trip() {
        assert_eq!(format_number(3.0), "3");
        assert_eq!(format_number(-0.0), "0");
        assert_eq!(format_number(0.8f32 as f64), "0.8");
        assert_eq!(format_number(0.1), "0.1");
        assert_eq!(format_number(f64::NAN), "null");
        assert_eq!(format_number(1e-30f32 as f64), "1e-30");
        let mut x = 1u32;
        for _ in 0..10_000 {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            let f = f32::from_bits(x);
            if !f.is_finite() {
                continue;
            }
            let text = format_number(f as f64);
            let back = parse(&text).unwrap().as_f64().unwrap() as f32;
            assert_eq!(back.to_bits(), if f == 0.0 { 0 } else { f.to_bits() }, "{f} → {text}");
        }
        for v in [0.1f64, 1.0 / 3.0, 123_456.789, 1e300, -2.5e-300] {
            assert_eq!(parse(&format_number(v)).unwrap().as_f64(), Some(v));
        }
    }

    #[test]
    fn pretty_layout() {
        let v = Json::Obj(vec![
            ("a".into(), Json::from(vec![1u32, 2])),
            ("b".into(), Json::Arr(vec![Json::from(vec![1u32]), Json::Obj(vec![])])),
            ("c".into(), Json::Arr(vec![])),
        ]);
        assert_eq!(v.pretty(), "{\n  \"a\": [1, 2],\n  \"b\": [\n    [1],\n    {}\n  ],\n  \"c\": []\n}\n");
        assert_eq!(parse(&v.pretty()).unwrap(), v);
        assert_eq!(parse(&v.compact()).unwrap(), v);
    }
}
