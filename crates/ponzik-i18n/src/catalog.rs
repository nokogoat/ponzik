//! Lecture des catalogues au format gettext `.po` :
//!
//! ```text
//! msgid ""
//! msgstr ""
//! "Language: fr\n"
//! "Language-Name: Français\n"
//! "Plural-Forms: nplurals=2; plural=(n > 1);\n"
//! "X-Decimal-Separator: ,\n"
//!
//! #: gui/app.rs
//! msgctxt "verb"
//! msgid "Open"
//! msgstr "Ouvrir"
//!
//! msgid "{} note"
//! msgid_plural "{} notes"
//! msgstr[0] "{} note"
//! msgstr[1] "{} notes"
//! ```
//!
//! Les catalogues des utilisateurs passent par ici aussi : la lecture ne
//! panique jamais. Ce qui ne se lit pas est noté dans `errors` (montré dans
//! l'interface) et ignoré ; les entrées vides, marquées `#, fuzzy` ou dont
//! les champs `{}` ne correspondent pas à l'anglais retombent sur l'anglais.

use std::collections::{HashMap, HashSet};

use crate::fmt::placeholders;

#[derive(Clone, Debug)]
pub struct Catalog {
    map: HashMap<String, String>,
    /// Clés traduites, dans l'ordre du fichier.
    order: Vec<String>,
    /// Toutes les clés du fichier, traduites ou non (mise à jour des modèles).
    ids: Vec<String>,
    id_set: HashSet<String>,
    headers: Vec<(String, String)>,
    /// Nom de la langue dans cette langue (en-tête `Language-Name`).
    pub name: Option<String>,
    /// Code de la langue (en-tête `Language`).
    pub language: Option<String>,
    pub decimal: char,
    /// Règle du pluriel (`Plural-Forms`) ; anglaise (`n != 1`) par défaut.
    pub plural: Plural,
    /// Lignes illisibles, doublons, champs `{}` différents : signalés à l'utilisateur.
    pub errors: Vec<String>,
}

impl Default for Catalog {
    fn default() -> Self {
        Catalog {
            map: HashMap::new(),
            order: Vec::new(),
            ids: Vec::new(),
            id_set: HashSet::new(),
            headers: Vec::new(),
            name: None,
            language: None,
            decimal: '.',
            plural: Plural::default(),
            errors: Vec::new(),
        }
    }
}

/// Lit une chaîne entre guillemets avec ses échappements C.
fn unquote(s: &str, errors: &mut Vec<String>, line: usize) -> Option<String> {
    let s = s.trim();
    let Some(inner) = s.strip_prefix('"').and_then(|r| r.strip_suffix('"')).filter(|_| s.len() >= 2) else {
        errors.push(format!("line {line}: expected a quoted string, found {s:?}"));
        return None;
    };
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '"' {
            errors.push(format!("line {line}: unescaped quote inside a string (write \\\")"));
            out.push(c);
            continue;
        }
        if c != '\\' {
            out.push(c);
            continue;
        }
        let Some(e) = chars.next() else {
            errors.push(format!("line {line}: string ends with a backslash"));
            break;
        };
        match e {
            'n' => out.push('\n'),
            't' => out.push('\t'),
            'r' => out.push('\r'),
            'a' => out.push('\u{7}'),
            'b' => out.push('\u{8}'),
            'f' => out.push('\u{c}'),
            'v' => out.push('\u{b}'),
            '"' | '\\' | '\'' | '?' => out.push(e),
            'x' => {
                let mut v = 0u32;
                let mut n = 0;
                while let Some(d) = chars.peek().and_then(|c| c.to_digit(16)) {
                    v = v.saturating_mul(16).saturating_add(d);
                    n += 1;
                    chars.next();
                }
                match char::from_u32(v).filter(|_| n > 0) {
                    Some(c) => out.push(c),
                    None => errors.push(format!("line {line}: bad \\x escape")),
                }
            }
            '0'..='7' => {
                let mut v = e.to_digit(8).unwrap_or(0);
                for _ in 0..2 {
                    match chars.peek().and_then(|c| c.to_digit(8)) {
                        Some(d) => {
                            v = v * 8 + d;
                            chars.next();
                        }
                        None => break,
                    }
                }
                out.push(char::from_u32(v).unwrap_or('\u{fffd}'));
            }
            other => {
                errors.push(format!("line {line}: unknown escape \\{other}"));
                out.push(other);
            }
        }
    }
    Some(out)
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Field {
    None,
    Ctxt,
    Id,
    IdPlural,
    Str(usize),
}

/// Entrée en cours de lecture.
#[derive(Default)]
struct Entry {
    ctxt: Option<String>,
    id: Option<String>,
    id_plural: Option<String>,
    strs: Vec<(usize, String)>,
    fuzzy: bool,
    line: usize,
}

impl Entry {
    fn started(&self) -> bool {
        self.ctxt.is_some() || self.id.is_some() || !self.strs.is_empty()
    }
}

impl Catalog {
    pub fn parse(src: &str) -> Catalog {
        let mut cat = Catalog::default();
        let src = src.strip_prefix('\u{feff}').unwrap_or(src);
        let mut e = Entry::default();
        let mut field = Field::None;
        for (n, raw) in src.lines().enumerate() {
            let n = n + 1;
            let line = raw.trim();
            if line.is_empty() {
                continue;
            }
            if let Some(comment) = line.strip_prefix('#') {
                // Un commentaire après une traduction commence l'entrée suivante.
                if !e.strs.is_empty() {
                    cat.flush(std::mem::take(&mut e));
                    field = Field::None;
                }
                if let Some(flags) = comment.strip_prefix(',')
                    && flags.split(',').any(|f| f.trim() == "fuzzy")
                {
                    e.fuzzy = true;
                }
                continue;
            }
            if line.starts_with('"') {
                let Some(text) = unquote(line, &mut cat.errors, n) else { continue };
                let slot = match field {
                    Field::Ctxt => e.ctxt.as_mut(),
                    Field::Id => e.id.as_mut(),
                    Field::IdPlural => e.id_plural.as_mut(),
                    Field::Str(i) => e.strs.iter_mut().rev().find(|(k, _)| *k == i).map(|(_, s)| s),
                    Field::None => None,
                };
                match slot {
                    Some(s) => s.push_str(&text),
                    None => cat.errors.push(format!("line {n}: string outside of an entry")),
                }
                continue;
            }
            let split = line.find(|c: char| c.is_whitespace() || c == '"').unwrap_or(line.len());
            let (kw, rest) = line.split_at(split);
            let next = match kw {
                "msgctxt" => Field::Ctxt,
                "msgid" => Field::Id,
                "msgid_plural" => Field::IdPlural,
                "msgstr" => Field::Str(0),
                _ => match kw.strip_prefix("msgstr[").and_then(|r| r.strip_suffix(']')).and_then(|i| i.parse().ok()) {
                    Some(i) if i < 16 => Field::Str(i),
                    _ => {
                        cat.errors.push(format!("line {n}: unknown line {:?}", truncate(line, 40)));
                        continue;
                    }
                },
            };
            let Some(text) = unquote(rest, &mut cat.errors, n) else {
                // Le reste de l'entrée ne doit pas se coller ailleurs.
                field = Field::None;
                continue;
            };
            match next {
                Field::Ctxt | Field::Id => {
                    // Fin de l'entrée précédente (complète, ou un msgid sans msgstr).
                    if !e.strs.is_empty() || e.id.is_some() {
                        cat.flush(std::mem::take(&mut e));
                    }
                    if !e.started() {
                        e.line = n;
                    }
                    if next == Field::Ctxt {
                        e.ctxt = Some(text);
                    } else {
                        e.id = Some(text);
                    }
                }
                Field::IdPlural => e.id_plural = Some(text),
                Field::Str(i) => e.strs.push((i, text)),
                Field::None => {}
            }
            field = next;
        }
        cat.flush(e);
        cat
    }

    fn flush(&mut self, e: Entry) {
        if !e.started() {
            return;
        }
        let Some(id) = e.id else {
            self.errors.push(format!("line {}: msgstr without msgid", e.line));
            return;
        };
        if e.strs.is_empty() {
            self.errors.push(format!("line {}: msgid without msgstr", e.line));
            return;
        }
        let str_at = |i: usize| e.strs.iter().find(|(k, _)| *k == i).map(|(_, s)| s.as_str());
        if id.is_empty() && e.ctxt.is_none() {
            self.set_header(str_at(0).unwrap_or(""));
            return;
        }
        let key = |id: &str| match &e.ctxt {
            Some(c) => format!("{c}\u{4}{id}"),
            None => id.to_string(),
        };
        // Pluriels : `tr_n` n'a que deux formes, « comme 1 » et « les autres » :
        // msgid prend la forme de n = 1, msgid_plural celle de n = 5.
        let mut pairs = Vec::with_capacity(2);
        match &e.id_plural {
            None => pairs.push((key(&id), str_at(0).unwrap_or(""))),
            Some(p) => {
                let form = |n: u64| usize::try_from(self.plural.index(n)).ok().and_then(str_at);
                pairs.push((key(&id), form(1).or_else(|| str_at(0)).unwrap_or("")));
                pairs.push((key(p), form(5).or_else(|| str_at(1)).or_else(|| str_at(0)).unwrap_or("")));
            }
        }
        for (k, tx) in pairs {
            if !self.id_set.insert(k.clone()) {
                self.errors.push(format!("line {}: duplicate msgid {:?}", e.line, truncate(&k, 40)));
                continue;
            }
            self.ids.push(k.clone());
            if tx.is_empty() || e.fuzzy {
                continue;
            }
            let src = k.rsplit('\u{4}').next().unwrap_or(&k);
            if placeholders(src) != placeholders(tx) {
                self.errors.push(format!("line {}: the {{}} fields of {:?} differ from the English text", e.line, truncate(src, 40)));
                continue;
            }
            self.order.push(k.clone());
            self.map.insert(k, tx.to_string());
        }
    }

    fn set_header(&mut self, text: &str) {
        for line in text.lines() {
            let Some((k, v)) = line.split_once(':') else { continue };
            let (k, v) = (k.trim(), v.trim());
            match self.headers.iter_mut().find(|(h, _)| h.eq_ignore_ascii_case(k)) {
                Some(h) => h.1 = v.to_string(),
                None => self.headers.push((k.to_string(), v.to_string())),
            }
        }
        self.apply_headers();
    }

    fn apply_headers(&mut self) {
        let get = |name: &str| self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str());
        let some = |v: Option<&str>| v.filter(|v| !v.is_empty()).map(str::to_string);
        self.name = some(get("Language-Name"));
        self.language = some(get("Language"));
        self.decimal = get("X-Decimal-Separator").and_then(|v| v.chars().next()).unwrap_or('.');
        self.plural = match get("Plural-Forms") {
            None => Plural::default(),
            Some(v) => match Plural::from_header(v) {
                Some(p) => p,
                None => {
                    self.errors.push(format!("Plural-Forms header not understood: {v:?}"));
                    Plural::default()
                }
            },
        };
    }

    /// Valeur d'un en-tête (`Language`, `Plural-Forms`…).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    /// `self` complété ou corrigé par `top` : ses traductions et en-têtes
    /// remplacent ceux de `self` (un utilisateur qui corrige le français).
    pub fn overlay(&self, top: &Catalog) -> Catalog {
        let mut out = self.clone();
        for k in &top.ids {
            if out.id_set.insert(k.clone()) {
                out.ids.push(k.clone());
            }
        }
        for k in &top.order {
            let tx = top.map[k].clone();
            if out.map.insert(k.clone(), tx).is_none() {
                out.order.push(k.clone());
            }
        }
        for (k, v) in &top.headers {
            match out.headers.iter_mut().find(|(h, _)| h.eq_ignore_ascii_case(k)) {
                Some(h) => h.1 = v.clone(),
                None => out.headers.push((k.clone(), v.clone())),
            }
        }
        out.errors = Vec::new();
        out.apply_headers();
        out.errors.extend(top.errors.iter().cloned());
        out
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.map.get(key).map(String::as_str)
    }

    pub fn contains(&self, key: &str) -> bool {
        self.map.contains_key(key)
    }

    /// Entrées traduites dans l'ordre du fichier (clé anglaise, traduction).
    pub fn entries(&self) -> impl Iterator<Item = (&str, &str)> + '_ {
        self.order.iter().map(|k| (k.as_str(), self.map[k].as_str()))
    }

    /// Toutes les clés du fichier, même sans traduction.
    pub fn ids(&self) -> impl Iterator<Item = &str> + '_ {
        self.ids.iter().map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }
}

fn truncate(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

/// Règle du pluriel de gettext : `plural=` est une expression C sur `n`
/// donnant l'indice de la forme. `tr_n` n'a que deux formes (une / les
/// autres) : la forme 0 est le singulier.
#[derive(Clone, Debug, PartialEq)]
pub struct Plural {
    pub forms: u32,
    expr: Expr,
}

impl Default for Plural {
    fn default() -> Self {
        Plural { forms: 2, expr: Expr::Bin(Op::Ne, Box::new(Expr::N), Box::new(Expr::Num(1))) }
    }
}

impl Plural {
    /// `nplurals=2; plural=(n > 1);`
    pub fn from_header(v: &str) -> Option<Plural> {
        let mut forms = None;
        let mut expr = None;
        for part in v.split(';') {
            let Some((k, val)) = part.split_once('=') else { continue };
            match k.trim() {
                "nplurals" => forms = val.trim().parse().ok(),
                // `plural=` peut contenir `==` : on reprend tout après le premier `=`.
                "plural" => expr = Some(parse_expr(val.trim())?),
                _ => {}
            }
        }
        Some(Plural { forms: forms.filter(|&f| (1..=16).contains(&f))?, expr: expr? })
    }

    /// Indice de la forme pour `n`.
    pub fn index(&self, n: u64) -> u64 {
        self.expr.eval(n)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Op {
    Or,
    And,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}

#[derive(Clone, Debug, PartialEq)]
enum Expr {
    N,
    Num(u64),
    Not(Box<Expr>),
    Bin(Op, Box<Expr>, Box<Expr>),
    If(Box<Expr>, Box<Expr>, Box<Expr>),
}

impl Expr {
    fn eval(&self, n: u64) -> u64 {
        match self {
            Expr::N => n,
            Expr::Num(v) => *v,
            Expr::Not(e) => u64::from(e.eval(n) == 0),
            Expr::If(c, a, b) => {
                if c.eval(n) != 0 {
                    a.eval(n)
                } else {
                    b.eval(n)
                }
            }
            Expr::Bin(op, a, b) => {
                let x = a.eval(n);
                // Court-circuit comme en C.
                match op {
                    Op::Or => return u64::from(x != 0 || b.eval(n) != 0),
                    Op::And => return u64::from(x != 0 && b.eval(n) != 0),
                    _ => {}
                }
                let y = b.eval(n);
                match op {
                    Op::Eq => u64::from(x == y),
                    Op::Ne => u64::from(x != y),
                    Op::Lt => u64::from(x < y),
                    Op::Le => u64::from(x <= y),
                    Op::Gt => u64::from(x > y),
                    Op::Ge => u64::from(x >= y),
                    Op::Add => x.wrapping_add(y),
                    Op::Sub => x.wrapping_sub(y),
                    Op::Mul => x.wrapping_mul(y),
                    Op::Div => x.checked_div(y).unwrap_or(0),
                    Op::Rem => x.checked_rem(y).unwrap_or(0),
                    Op::Or | Op::And => 0,
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Tok {
    N,
    Num(u64),
    Op(&'static str),
}

fn tokens(s: &str) -> Option<Vec<Tok>> {
    const OPS: [&str; 19] = ["||", "&&", "==", "!=", "<=", ">=", "<", ">", "+", "-", "*", "/", "%", "!", "?", ":", "(", ")", ";"];
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(c) = rest.chars().next() {
        if c.is_whitespace() {
            rest = &rest[c.len_utf8()..];
        } else if c == 'n' {
            out.push(Tok::N);
            rest = &rest[1..];
        } else if c.is_ascii_digit() {
            let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
            out.push(Tok::Num(rest[..end].parse().ok()?));
            rest = &rest[end..];
        } else {
            let op = OPS.iter().find(|op| rest.starts_with(**op))?;
            out.push(Tok::Op(op));
            rest = &rest[op.len()..];
        }
        if out.len() > 512 {
            return None;
        }
    }
    // `plural=(n > 1);` : le `;` final termine l'expression.
    while out.last() == Some(&Tok::Op(";")) {
        out.pop();
    }
    Some(out)
}

fn parse_expr(s: &str) -> Option<Expr> {
    let toks = tokens(s)?;
    let mut p = Parser { toks: &toks, i: 0, depth: 0 };
    let e = p.ternary()?;
    (p.i == toks.len()).then_some(e)
}

struct Parser<'a> {
    toks: &'a [Tok],
    i: usize,
    depth: usize,
}

impl Parser<'_> {
    fn eat(&mut self, op: &'static str) -> bool {
        let hit = self.toks.get(self.i) == Some(&Tok::Op(op));
        self.i += usize::from(hit);
        hit
    }

    fn ternary(&mut self) -> Option<Expr> {
        self.depth += 1;
        if self.depth > 64 {
            return None;
        }
        let c = self.binary(0)?;
        let e = if self.eat("?") {
            let a = self.ternary()?;
            if !self.eat(":") {
                return None;
            }
            let b = self.ternary()?;
            Expr::If(Box::new(c), Box::new(a), Box::new(b))
        } else {
            c
        };
        self.depth -= 1;
        Some(e)
    }

    /// Précédences de C, de la plus faible à la plus forte.
    fn binary(&mut self, level: usize) -> Option<Expr> {
        const LEVELS: [&[(&str, Op)]; 6] = [
            &[("||", Op::Or)],
            &[("&&", Op::And)],
            &[("==", Op::Eq), ("!=", Op::Ne)],
            &[("<=", Op::Le), (">=", Op::Ge), ("<", Op::Lt), (">", Op::Gt)],
            &[("+", Op::Add), ("-", Op::Sub)],
            &[("*", Op::Mul), ("/", Op::Div), ("%", Op::Rem)],
        ];
        let Some(ops) = LEVELS.get(level) else { return self.unary() };
        let mut left = self.binary(level + 1)?;
        while let Some(Tok::Op(t)) = self.toks.get(self.i)
            && let Some(&(_, op)) = ops.iter().find(|(s, _)| s == t)
        {
            self.i += 1;
            let right = self.binary(level + 1)?;
            left = Expr::Bin(op, Box::new(left), Box::new(right));
        }
        Some(left)
    }

    fn unary(&mut self) -> Option<Expr> {
        let t = *self.toks.get(self.i)?;
        self.i += 1;
        match t {
            Tok::N => Some(Expr::N),
            Tok::Num(v) => Some(Expr::Num(v)),
            Tok::Op("!") => Some(Expr::Not(Box::new(self.unary()?))),
            Tok::Op("(") => {
                let e = self.ternary()?;
                self.eat(")").then_some(e)
            }
            Tok::Op(_) => None,
        }
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
        assert_eq!(c.name.as_deref(), Some("Test"));
        assert_eq!(c.decimal, ',');
        assert_eq!(c.plural.index(0), 0);
        assert_eq!(c.plural.index(2), 1);
        assert_eq!(c.get("Open"), Some("Ouvrir"));
        assert_eq!(c.get("state\u{4}Open"), Some("Ouvert"));
        assert_eq!(c.get("Two lines"), Some("Deux \"lignes\"\n"));
        assert_eq!(c.get("Untranslated"), None);
        assert_eq!(c.len(), 3);
        assert_eq!(c.ids().count(), 4);
    }

    #[test]
    fn reports_duplicates_and_garbage() {
        let c = Catalog::parse("msgid \"A\"\nmsgstr \"B\"\nmsgid \"A\"\nmsgstr \"C\"\nnonsense\n");
        assert_eq!(c.errors.len(), 2, "{:?}", c.errors);
        assert_eq!(c.get("A"), Some("B"));
    }

    #[test]
    fn bom_crlf_escapes_plurals_and_fuzzy() {
        let src = "\u{feff}msgid \"\"\r\nmsgstr \"\"\r\n\"Language: es\\n\"\r\n\"Language-Name: Español\\n\"\r\n\r\n\
            msgid \"{} note\"\r\nmsgid_plural \"{} notes\"\r\nmsgstr[0] \"{} nota\"\r\nmsgstr[1] \"{} notas\"\r\n\r\n\
            #, fuzzy\r\nmsgid \"Save\"\r\nmsgstr \"Guardar\"\r\n\r\n\
            msgid \"Tab\\there\"\r\nmsgstr \"\\x41\\101\\t\\\\ \\'ok\\'\"\r\n";
        let c = Catalog::parse(src);
        assert!(c.errors.is_empty(), "{:?}", c.errors);
        assert_eq!(c.language.as_deref(), Some("es"));
        assert_eq!(c.name.as_deref(), Some("Español"));
        assert_eq!(c.get("{} note"), Some("{} nota"));
        assert_eq!(c.get("{} notes"), Some("{} notas"));
        assert_eq!(c.get("Save"), None, "fuzzy entries are not used");
        assert!(c.ids().any(|k| k == "Save"));
        assert_eq!(c.get("Tab\there"), Some("AA\t\\ 'ok'"));
    }

    #[test]
    fn malformed_input_never_panics() {
        let inputs = [
            "",
            "\"",
            "msgid",
            "msgid \"unterminated\nmsgstr \"x\"",
            "msgstr \"orphan\"",
            "msgid \"a\"\nmsgid \"b\"\nmsgstr \"c\"",
            "msgid \"\\\"\nmsgstr \"\\",
            "msgstr[99] \"x\"",
            "msgid \"{}\"\nmsgstr \"no field\"",
            "msgid \"\"\nmsgstr \"Plural-Forms: nplurals=2; plural=((((n;\\n\"",
            "\"continuation\" \"first\"",
            "msgctxt \"c\"\n\n\n",
            "\u{feff}\u{feff}msgid \"x\"\nmsgstr \"y\"",
            "msgid \"é\\x\"\nmsgstr \"\\777\"",
        ];
        for src in inputs {
            let c = Catalog::parse(src);
            let _ = (c.len(), c.errors.len());
        }
        let c = Catalog::parse("msgid \"{} files\"\nmsgstr \"fichiers\"\n");
        assert_eq!(c.get("{} files"), None);
        assert_eq!(c.errors.len(), 1);
        let c = Catalog::parse("msgid \"a\"\nmsgid \"b\"\nmsgstr \"c\"\n");
        assert_eq!(c.get("b"), Some("c"));
        assert_eq!(c.errors.len(), 1, "{:?}", c.errors);
    }

    #[test]
    fn plural_rules() {
        let p = |s: &str| Plural::from_header(s).expect(s);
        let en = p("nplurals=2; plural=(n != 1);");
        assert_eq!((en.index(0), en.index(1), en.index(2)), (1, 0, 1));
        let fr = p("nplurals=2; plural=(n > 1);");
        assert_eq!((fr.index(0), fr.index(1), fr.index(2)), (0, 0, 1));
        let ja = p("nplurals=1; plural=0;");
        assert_eq!(ja.index(5), 0);
        let ru = p("nplurals=3; plural=(n%10==1 && n%100!=11 ? 0 : n%10>=2 && n%10<=4 && (n%100<10 || n%100>=20) ? 1 : 2);");
        assert_eq!([1, 2, 5, 11, 21, 22, 25].map(|n| ru.index(n)), [0, 1, 2, 2, 0, 1, 2]);
        assert!(Plural::from_header("nplurals=2; plural=n +;").is_none());
        assert!(Plural::from_header("plural=n").is_none());
        assert_eq!(p("nplurals=2; plural=n/0;").index(3), 0);
    }

    #[test]
    fn overlay_overrides_and_extends() {
        let base = Catalog::parse("msgid \"\"\nmsgstr \"Language-Name: Base\\nX-Decimal-Separator: ,\\n\"\nmsgid \"A\"\nmsgstr \"a\"\nmsgid \"B\"\nmsgstr \"b\"\n");
        let top = Catalog::parse("msgid \"\"\nmsgstr \"Language-Name: Top\\n\"\nmsgid \"B\"\nmsgstr \"bee\"\nmsgid \"C\"\nmsgstr \"c\"\nmsgid \"A\"\nmsgstr \"\"\n");
        let m = base.overlay(&top);
        assert_eq!(m.name.as_deref(), Some("Top"));
        assert_eq!(m.decimal, ',');
        assert_eq!((m.get("A"), m.get("B"), m.get("C")), (Some("a"), Some("bee"), Some("c")));
        assert_eq!(m.len(), 3);
    }
}
