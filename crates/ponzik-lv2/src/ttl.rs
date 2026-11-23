//! A Turtle (TTL) reader, enough for LV2 bundles: `@prefix` / `@base` and
//! their SPARQL forms, IRIs resolved against the file, prefixed names,
//! literals (short and long strings, escapes, language tags, datatypes,
//! numbers, booleans), `a`, `;` and `,`, blank nodes (`_:x` and `[ … ]`) and
//! collections `( … )`.
//!
//! Triples land in a [`Graph`], indexed by subject. Several files go into
//! the same graph (a bundle's manifest, then the files it points to); blank
//! nodes stay distinct between files.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
pub const RDF_FIRST: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#first";
pub const RDF_REST: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#rest";
pub const RDF_NIL: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#nil";
const XSD: &str = "http://www.w3.org/2001/XMLSchema#";

/// A node of the graph.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Term {
    Iri(String),
    Blank(u32),
    /// `datatype` and `lang` are empty when absent.
    Literal { value: String, datatype: String, lang: String },
}

impl Term {
    pub fn iri(s: &str) -> Term {
        Term::Iri(s.to_string())
    }

    pub fn as_iri(&self) -> Option<&str> {
        match self {
            Term::Iri(s) => Some(s),
            _ => None,
        }
    }

    /// The text of a literal.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Term::Literal { value, .. } => Some(value),
            _ => None,
        }
    }

    /// A number (or a boolean, as 0 and 1) written as a literal.
    pub fn as_f64(&self) -> Option<f64> {
        let v = self.as_str()?.trim();
        match v {
            "true" => Some(1.0),
            "false" => Some(0.0),
            _ => v.parse::<f64>().ok().filter(|x| x.is_finite()),
        }
    }

    pub fn as_f32(&self) -> Option<f32> {
        self.as_f64().map(|v| v as f32)
    }
}

pub struct Triple {
    pub s: Term,
    pub p: String,
    pub o: Term,
}

/// Triples of one or more Turtle files.
#[derive(Default)]
pub struct Graph {
    triples: Vec<Triple>,
    by_subject: HashMap<Term, Vec<usize>>,
    next_blank: u32,
    /// Files already read (by path), so `rdfs:seeAlso` loops end.
    loaded: HashSet<PathBuf>,
}

impl Graph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.triples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.triples.is_empty()
    }

    fn add(&mut self, s: Term, p: String, o: Term) {
        let i = self.triples.len();
        self.by_subject.entry(s.clone()).or_default().push(i);
        self.triples.push(Triple { s, p, o });
    }

    /// Reads Turtle `text`, its relative IRIs resolved against `base`. On a
    /// syntax error the triples read before it stay.
    pub fn parse(&mut self, text: &str, base: &str) -> Result<(), String> {
        let mut p = Parser {
            src: text.as_bytes(),
            pos: 0,
            base: base.to_string(),
            prefixes: HashMap::new(),
            blanks: HashMap::new(),
            graph: self,
        };
        p.document()
    }

    /// Reads a file once (later calls for the same file do nothing).
    pub fn load_file(&mut self, path: &Path) -> Result<(), String> {
        let key = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if !self.loaded.insert(key) {
            return Ok(());
        }
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        self.parse(&text, &path_to_uri(path)).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Triples whose subject is `s`.
    pub fn about<'a>(&'a self, s: &Term) -> impl Iterator<Item = &'a Triple> + 'a {
        self.by_subject.get(s).into_iter().flatten().map(|&i| &self.triples[i])
    }

    pub fn objects<'a>(&'a self, s: &Term, p: &'a str) -> impl Iterator<Item = &'a Term> + 'a {
        self.about(s).filter(move |t| t.p == p).map(|t| &t.o)
    }

    pub fn object(&self, s: &Term, p: &str) -> Option<&Term> {
        self.about(s).find(|t| t.p == p).map(|t| &t.o)
    }

    pub fn has(&self, s: &Term, p: &str, o: &str) -> bool {
        self.about(s).any(|t| t.p == p && t.o.as_iri() == Some(o))
    }

    pub fn has_type(&self, s: &Term, class: &str) -> bool {
        self.has(s, RDF_TYPE, class)
    }

    /// Subjects of the triples `? p o`.
    pub fn subjects(&self, p: &str, o: &Term) -> Vec<Term> {
        let mut out: Vec<Term> = Vec::new();
        for t in &self.triples {
            if t.p == p && t.o == *o && !out.contains(&t.s) {
                out.push(t.s.clone());
            }
        }
        out
    }

    /// Items of an RDF collection (`( … )`).
    pub fn list(&self, head: &Term) -> Vec<Term> {
        let mut out = Vec::new();
        let mut cur = head.clone();
        while cur.as_iri() != Some(RDF_NIL) && out.len() < 100_000 {
            let Some(first) = self.object(&cur, RDF_FIRST) else { break };
            out.push(first.clone());
            match self.object(&cur, RDF_REST) {
                Some(next) => cur = next.clone(),
                None => break,
            }
        }
        out
    }
}

// --- IRIs and paths ------------------------------------------------------------

fn has_scheme(iri: &str) -> bool {
    let Some(colon) = iri.find(':') else { return false };
    let scheme = &iri[..colon];
    !scheme.is_empty()
        && scheme.as_bytes()[0].is_ascii_alphabetic()
        && scheme.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'-' || b == b'.')
}

/// Removes `.` and `..` segments of a path.
fn remove_dots(path: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let segments: Vec<&str> = path.split('/').collect();
    for (i, seg) in segments.iter().enumerate() {
        let last = i + 1 == segments.len();
        match *seg {
            "." => {
                if last {
                    out.push("");
                }
            }
            ".." => {
                if out.len() > 1 {
                    out.pop();
                }
                if last {
                    out.push("");
                }
            }
            s => out.push(s),
        }
    }
    out.join("/")
}

/// `rel` resolved against `base` (RFC 3986, without the rare corners).
pub fn resolve(base: &str, rel: &str) -> String {
    if has_scheme(rel) {
        return rel.to_string();
    }
    let base_nofrag = base.split('#').next().unwrap_or(base);
    if rel.is_empty() {
        return base_nofrag.to_string();
    }
    if rel.starts_with('#') {
        return format!("{base_nofrag}{rel}");
    }
    // Scheme and authority of the base.
    let (scheme, rest) = match base_nofrag.find(':') {
        Some(i) => (&base_nofrag[..=i], &base_nofrag[i + 1..]),
        None => ("", base_nofrag),
    };
    let (authority, path) = if let Some(r) = rest.strip_prefix("//") {
        let end = r.find('/').unwrap_or(r.len());
        (&rest[..end + 2], &r[end..])
    } else {
        ("", rest)
    };
    if rel.starts_with("//") {
        return format!("{scheme}{rel}");
    }
    let path = path.split('?').next().unwrap_or(path);
    let merged = if rel.starts_with('/') {
        rel.to_string()
    } else if rel.starts_with('?') {
        format!("{path}{rel}")
    } else {
        let dir = match path.rfind('/') {
            Some(i) => &path[..=i],
            None => "/",
        };
        format!("{dir}{rel}")
    };
    let (p, q) = match merged.find(['?', '#']) {
        Some(i) => (&merged[..i], &merged[i..]),
        None => (merged.as_str(), ""),
    };
    format!("{scheme}{authority}{}{q}", remove_dots(p))
}

/// `file://` URI of a path (with a trailing `/` for a directory if `dir`).
pub fn path_to_uri(path: &Path) -> String {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map(|d| d.join(path)).unwrap_or_else(|_| path.to_path_buf())
    };
    let mut s = abs.to_string_lossy().replace('\\', "/");
    // `canonicalize` on Windows: `//?/C:/…`.
    if let Some(r) = s.strip_prefix("//?/") {
        s = r.to_string();
    }
    let mut out = String::from("file://");
    if !s.starts_with('/') {
        out.push('/');
    }
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~:@!$&'()*+,;=".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Path of a `file://` URI; `None` for another scheme.
pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    // `file://host/path`: only the local host.
    let rest = if rest.starts_with('/') { rest } else { rest.strip_prefix("localhost")? };
    let rest = rest.split('#').next().unwrap_or(rest);
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Some(v) = std::str::from_utf8(&bytes[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    let mut s = String::from_utf8_lossy(&out).into_owned();
    // `/C:/x` on Windows.
    if cfg!(windows) && s.len() > 2 && s.as_bytes()[0] == b'/' && s.as_bytes()[2] == b':' {
        s.remove(0);
    }
    Some(PathBuf::from(s))
}

// --- Parser --------------------------------------------------------------------

struct Parser<'a> {
    src: &'a [u8],
    pos: usize,
    base: String,
    prefixes: HashMap<String, String>,
    blanks: HashMap<String, u32>,
    graph: &'a mut Graph,
}

fn is_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.' || b == b':' || b == b'%' || b == b'\\' || b >= 0x80
}

impl Parser<'_> {
    fn err(&self, msg: &str) -> String {
        let line = self.src[..self.pos.min(self.src.len())].iter().filter(|&&b| b == b'\n').count() + 1;
        format!("line {line}: {msg}")
    }

    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn peek_at(&self, n: usize) -> Option<u8> {
        self.src.get(self.pos + n).copied()
    }

    fn starts(&self, s: &str) -> bool {
        self.src[self.pos..].starts_with(s.as_bytes())
    }

    /// Skips white space and comments.
    fn ws(&mut self) {
        while let Some(b) = self.peek() {
            if b.is_ascii_whitespace() {
                self.pos += 1;
            } else if b == b'#' {
                while let Some(b) = self.peek() {
                    self.pos += 1;
                    if b == b'\n' {
                        break;
                    }
                }
            } else {
                break;
            }
        }
    }

    fn expect(&mut self, b: u8) -> Result<(), String> {
        self.ws();
        if self.peek() == Some(b) {
            self.pos += 1;
            Ok(())
        } else {
            Err(self.err(&format!("'{}' expected", b as char)))
        }
    }

    fn blank(&mut self) -> Term {
        let id = self.graph.next_blank;
        self.graph.next_blank += 1;
        Term::Blank(id)
    }

    fn document(&mut self) -> Result<(), String> {
        loop {
            self.ws();
            if self.peek().is_none() {
                return Ok(());
            }
            self.statement()?;
        }
    }

    /// A keyword (`PREFIX`, `BASE`) at the position, case-insensitive, as a whole word.
    fn keyword(&self, kw: &str) -> bool {
        let n = kw.len();
        self.src.len() >= self.pos + n
            && self.src[self.pos..self.pos + n].eq_ignore_ascii_case(kw.as_bytes())
            && self.src.get(self.pos + n).is_none_or(|b| b.is_ascii_whitespace())
    }

    fn statement(&mut self) -> Result<(), String> {
        if self.starts("@prefix") {
            self.pos += 7;
            self.prefix_decl()?;
            return self.expect(b'.');
        }
        if self.starts("@base") {
            self.pos += 5;
            self.ws();
            let iri = self.iriref()?;
            self.base = iri;
            return self.expect(b'.');
        }
        if self.keyword("PREFIX") {
            self.pos += 6;
            return self.prefix_decl();
        }
        if self.keyword("BASE") {
            self.pos += 4;
            self.ws();
            self.base = self.iriref()?;
            return Ok(());
        }
        self.triples()?;
        self.expect(b'.')
    }

    fn prefix_decl(&mut self) -> Result<(), String> {
        self.ws();
        let start = self.pos;
        while self.peek().is_some_and(|b| b != b':' && !b.is_ascii_whitespace()) {
            self.pos += 1;
        }
        let name = String::from_utf8_lossy(&self.src[start..self.pos]).into_owned();
        self.expect(b':')?;
        self.ws();
        let iri = self.iriref()?;
        self.prefixes.insert(name, iri);
        Ok(())
    }

    fn triples(&mut self) -> Result<(), String> {
        self.ws();
        if self.peek() == Some(b'[') {
            let s = self.blank_property_list()?;
            self.ws();
            if self.peek() != Some(b'.') {
                self.predicate_object_list(&s)?;
            }
            return Ok(());
        }
        let s = self.subject()?;
        self.predicate_object_list(&s)
    }

    fn subject(&mut self) -> Result<Term, String> {
        self.ws();
        match self.peek() {
            Some(b'<') => Ok(Term::Iri(self.iriref()?)),
            Some(b'(') => self.collection(),
            Some(b'_') if self.peek_at(1) == Some(b':') => Ok(self.blank_label()),
            Some(_) => Ok(Term::Iri(self.prefixed()?)),
            None => Err(self.err("subject expected")),
        }
    }

    fn predicate_object_list(&mut self, s: &Term) -> Result<(), String> {
        loop {
            self.ws();
            let p = self.verb()?;
            self.object_list(s, &p)?;
            self.ws();
            if self.peek() != Some(b';') {
                return Ok(());
            }
            // One or more `;`, possibly before the end of the list.
            while self.peek() == Some(b';') {
                self.pos += 1;
                self.ws();
            }
            if matches!(self.peek(), Some(b'.') | Some(b']') | None) {
                return Ok(());
            }
        }
    }

    fn verb(&mut self) -> Result<String, String> {
        self.ws();
        if self.peek() == Some(b'a') && self.peek_at(1).is_none_or(|b| !is_name_byte(b)) {
            self.pos += 1;
            return Ok(RDF_TYPE.to_string());
        }
        if self.peek() == Some(b'<') {
            return self.iriref();
        }
        self.prefixed()
    }

    fn object_list(&mut self, s: &Term, p: &str) -> Result<(), String> {
        loop {
            let o = self.object()?;
            self.graph.add(s.clone(), p.to_string(), o);
            self.ws();
            if self.peek() == Some(b',') {
                self.pos += 1;
            } else {
                return Ok(());
            }
        }
    }

    fn object(&mut self) -> Result<Term, String> {
        self.ws();
        match self.peek() {
            Some(b'<') => Ok(Term::Iri(self.iriref()?)),
            Some(b'[') => self.blank_property_list(),
            Some(b'(') => self.collection(),
            Some(b'"') | Some(b'\'') => self.string_literal(),
            Some(b'_') if self.peek_at(1) == Some(b':') => Ok(self.blank_label()),
            Some(b) if b.is_ascii_digit() || b == b'+' || b == b'-' || (b == b'.' && self.peek_at(1).is_some_and(|c| c.is_ascii_digit())) => {
                self.number()
            }
            Some(_) => {
                if self.word("true") || self.word("false") {
                    let v = if self.starts("true") { "true" } else { "false" };
                    self.pos += v.len();
                    return Ok(Term::Literal { value: v.into(), datatype: format!("{XSD}boolean"), lang: String::new() });
                }
                Ok(Term::Iri(self.prefixed()?))
            }
            None => Err(self.err("object expected")),
        }
    }

    fn word(&self, w: &str) -> bool {
        self.starts(w) && self.src.get(self.pos + w.len()).is_none_or(|&b| !is_name_byte(b) || b == b'.')
    }

    fn blank_label(&mut self) -> Term {
        self.pos += 2;
        let start = self.pos;
        while self.peek().is_some_and(|b| is_name_byte(b) && b != b':') {
            self.pos += 1;
        }
        while self.pos > start && self.src[self.pos - 1] == b'.' {
            self.pos -= 1;
        }
        let label = String::from_utf8_lossy(&self.src[start..self.pos]).into_owned();
        if let Some(&id) = self.blanks.get(&label) {
            return Term::Blank(id);
        }
        let t = self.blank();
        if let Term::Blank(id) = t {
            self.blanks.insert(label, id);
        }
        t
    }

    fn blank_property_list(&mut self) -> Result<Term, String> {
        self.expect(b'[')?;
        let b = self.blank();
        self.ws();
        if self.peek() != Some(b']') {
            self.predicate_object_list(&b)?;
        }
        self.expect(b']')?;
        Ok(b)
    }

    fn collection(&mut self) -> Result<Term, String> {
        self.expect(b'(')?;
        let mut items = Vec::new();
        loop {
            self.ws();
            match self.peek() {
                Some(b')') => {
                    self.pos += 1;
                    break;
                }
                None => return Err(self.err("')' expected")),
                _ => items.push(self.object()?),
            }
        }
        let mut head = Term::iri(RDF_NIL);
        for item in items.into_iter().rev() {
            let node = self.blank();
            self.graph.add(node.clone(), RDF_FIRST.into(), item);
            self.graph.add(node.clone(), RDF_REST.into(), head);
            head = node;
        }
        Ok(head)
    }

    /// `<…>`, resolved against the base.
    fn iriref(&mut self) -> Result<String, String> {
        self.ws();
        if self.peek() != Some(b'<') {
            return Err(self.err("IRI expected"));
        }
        self.pos += 1;
        let mut out = Vec::new();
        loop {
            match self.peek() {
                None => return Err(self.err("unterminated IRI")),
                Some(b'>') => {
                    self.pos += 1;
                    break;
                }
                Some(b'\\') => {
                    self.pos += 1;
                    let c = self.unicode_escape()?;
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                }
                Some(b) if b.is_ascii_whitespace() => return Err(self.err("space in an IRI")),
                Some(b) => {
                    out.push(b);
                    self.pos += 1;
                }
            }
        }
        let rel = String::from_utf8_lossy(&out).into_owned();
        Ok(resolve(&self.base, &rel))
    }

    /// After `\`: `uXXXX` or `UXXXXXXXX`.
    fn unicode_escape(&mut self) -> Result<char, String> {
        let n = match self.peek() {
            Some(b'u') => 4,
            Some(b'U') => 8,
            _ => return Err(self.err("bad escape")),
        };
        self.pos += 1;
        let hex = self.src.get(self.pos..self.pos + n).ok_or_else(|| self.err("bad escape"))?;
        let v = std::str::from_utf8(hex).ok().and_then(|h| u32::from_str_radix(h, 16).ok()).ok_or_else(|| self.err("bad escape"))?;
        self.pos += n;
        Ok(char::from_u32(v).unwrap_or('\u{FFFD}'))
    }

    /// `prefix:local`, expanded.
    fn prefixed(&mut self) -> Result<String, String> {
        let start = self.pos;
        while self.peek().is_some_and(is_name_byte) {
            if self.peek() == Some(b'\\') {
                self.pos += 1;
            }
            self.pos += 1;
        }
        // A name never ends with a dot: that one ends the statement.
        while self.pos > start && self.src[self.pos - 1] == b'.' {
            self.pos -= 1;
        }
        let tok = String::from_utf8_lossy(&self.src[start..self.pos.min(self.src.len())]).into_owned();
        let Some((prefix, local)) = tok.split_once(':') else {
            self.pos = start;
            return Err(self.err(&format!("unexpected '{}'", tok.chars().next().unwrap_or('?'))));
        };
        let Some(ns) = self.prefixes.get(prefix) else {
            return Err(self.err(&format!("unknown prefix '{prefix}:'")));
        };
        // Escapes of reserved characters in local names: `\-` → `-`.
        let mut local_clean = String::with_capacity(local.len());
        let mut chars = local.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                if let Some(n) = chars.next() {
                    local_clean.push(n);
                }
            } else {
                local_clean.push(c);
            }
        }
        Ok(format!("{ns}{local_clean}"))
    }

    fn number(&mut self) -> Result<Term, String> {
        let start = self.pos;
        if matches!(self.peek(), Some(b'+') | Some(b'-')) {
            self.pos += 1;
        }
        let digits = |p: &mut Self| {
            let s = p.pos;
            while p.peek().is_some_and(|b| b.is_ascii_digit()) {
                p.pos += 1;
            }
            p.pos > s
        };
        let mut kind = "integer";
        let int = digits(self);
        if self.peek() == Some(b'.') && self.peek_at(1).is_some_and(|b| b.is_ascii_digit()) {
            self.pos += 1;
            digits(self);
            kind = "decimal";
        } else if !int {
            return Err(self.err("number expected"));
        } else if self.peek() == Some(b'.') {
            // `2. ;` is not Turtle, but some files have it: a dot that
            // cannot end the statement belongs to the number.
            let mut k = self.pos + 1;
            while self.src.get(k).is_some_and(|b| b.is_ascii_whitespace()) {
                k += 1;
            }
            if matches!(self.src.get(k), Some(b';') | Some(b',') | Some(b']') | Some(b')')) {
                self.pos += 1;
                kind = "decimal";
            }
        }
        if matches!(self.peek(), Some(b'e') | Some(b'E')) {
            let save = self.pos;
            self.pos += 1;
            if matches!(self.peek(), Some(b'+') | Some(b'-')) {
                self.pos += 1;
            }
            if digits(self) {
                kind = "double";
            } else {
                self.pos = save;
            }
        }
        let value = String::from_utf8_lossy(&self.src[start..self.pos]).into_owned();
        Ok(Term::Literal { value, datatype: format!("{XSD}{kind}"), lang: String::new() })
    }

    fn string_literal(&mut self) -> Result<Term, String> {
        let q = self.peek().unwrap_or(b'"');
        let long = self.peek_at(1) == Some(q) && self.peek_at(2) == Some(q);
        self.pos += if long { 3 } else { 1 };
        let mut out: Vec<u8> = Vec::new();
        loop {
            let Some(b) = self.peek() else { return Err(self.err("unterminated string")) };
            if b == q {
                if !long {
                    self.pos += 1;
                    break;
                }
                if self.peek_at(1) == Some(q) && self.peek_at(2) == Some(q) {
                    // `""""` ends with a quote inside: the last three close.
                    while self.peek_at(3) == Some(q) {
                        out.push(q);
                        self.pos += 1;
                    }
                    self.pos += 3;
                    break;
                }
                out.push(b);
                self.pos += 1;
                continue;
            }
            if !long && (b == b'\n' || b == b'\r') {
                return Err(self.err("line break in a string"));
            }
            if b == b'\\' {
                self.pos += 1;
                let e = self.peek().ok_or_else(|| self.err("bad escape"))?;
                let c = match e {
                    b't' => '\t',
                    b'b' => '\u{8}',
                    b'n' => '\n',
                    b'r' => '\r',
                    b'f' => '\u{c}',
                    b'"' => '"',
                    b'\'' => '\'',
                    b'\\' => '\\',
                    b'u' | b'U' => {
                        let c = self.unicode_escape()?;
                        let mut buf = [0u8; 4];
                        out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                        continue;
                    }
                    _ => return Err(self.err("bad escape")),
                };
                self.pos += 1;
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                continue;
            }
            out.push(b);
            self.pos += 1;
        }
        let value = String::from_utf8_lossy(&out).into_owned();
        let mut datatype = String::new();
        let mut lang = String::new();
        if self.peek() == Some(b'@') {
            self.pos += 1;
            let start = self.pos;
            while self.peek().is_some_and(|b| b.is_ascii_alphanumeric() || b == b'-') {
                self.pos += 1;
            }
            lang = String::from_utf8_lossy(&self.src[start..self.pos]).into_owned();
        } else if self.starts("^^") {
            self.pos += 2;
            datatype = if self.peek() == Some(b'<') { self.iriref()? } else { self.prefixed()? };
        }
        Ok(Term::Literal { value, datatype, lang })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LV2: &str = "http://lv2plug.in/ns/lv2core#";

    #[test]
    fn iris_resolve_against_the_base() {
        let base = "file:///usr/lib/lv2/x.lv2/manifest.ttl";
        assert_eq!(resolve(base, "x.so"), "file:///usr/lib/lv2/x.lv2/x.so");
        assert_eq!(resolve(base, "../y.lv2/y.ttl"), "file:///usr/lib/lv2/y.lv2/y.ttl");
        assert_eq!(resolve(base, "./a/./b.ttl"), "file:///usr/lib/lv2/x.lv2/a/b.ttl");
        assert_eq!(resolve(base, "#frag"), "file:///usr/lib/lv2/x.lv2/manifest.ttl#frag");
        assert_eq!(resolve(base, "/abs.ttl"), "file:///abs.ttl");
        assert_eq!(resolve(base, "http://a.org/b#c"), "http://a.org/b#c");
        assert_eq!(resolve("http://a.org/p/q", "r"), "http://a.org/p/r");
        assert_eq!(resolve("http://a.org", "r"), "http://a.org/r");
        assert_eq!(resolve("urn:ponzik:test", "#x"), "urn:ponzik:test#x");
    }

    #[test]
    fn paths_and_uris_round_trip() {
        let p = Path::new("/tmp/a b/é#.lv2/x.ttl");
        let u = path_to_uri(p);
        assert_eq!(u, "file:///tmp/a%20b/%C3%A9%23.lv2/x.ttl");
        assert_eq!(uri_to_path(&u).unwrap(), p);
        assert_eq!(uri_to_path("http://x"), None);
    }

    #[test]
    fn a_plugin_description_is_read() {
        let ttl = r#"
@prefix lv2:  <http://lv2plug.in/ns/lv2core#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix rdf:  <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix doap: <http://usefulinc.com/ns/doap#> .
@prefix : <http://example.org/plug#> .
PREFIX ex: <http://example.org/ns#>

# a comment
:amp a lv2:Plugin, lv2:AmplifierPlugin ;
    doap:name "Amp \"one\"" , "Ampli"@fr ;
    lv2:binary <amp.so> ;
    lv2:optionalFeature lv2:hardRTCapable ;;
    ex:list ( 1 2.5 -3e2 ) ;
    ex:flag true ;
    ex:long """two
lines with "quotes" """ ;
    ex:typed "7"^^<http://www.w3.org/2001/XMLSchema#int> ;
    lv2:port [
        a lv2:InputPort , lv2:ControlPort ;
        lv2:index 0 ; lv2:symbol "gain" ; lv2:name 'Gain' ;
        lv2:default 1.0 ; lv2:minimum 0 ; lv2:maximum 2. ;
        lv2:scalePoint [ rdfs:label "Unity"; rdf:value 1 ] ;
    ] , _:out .
_:out a lv2:OutputPort, lv2:AudioPort ; lv2:index 1 .
[] a lv2:Feature ; rdfs:label "anon" .
"#;
        let mut g = Graph::new();
        g.parse(ttl, "file:///b/amp.lv2/amp.ttl").unwrap();
        let amp = Term::iri("http://example.org/plug#amp");
        assert!(g.has_type(&amp, &format!("{LV2}Plugin")));
        assert!(g.has_type(&amp, &format!("{LV2}AmplifierPlugin")));
        let names: Vec<_> = g.objects(&amp, "http://usefulinc.com/ns/doap#name").filter_map(Term::as_str).collect();
        assert_eq!(names, ["Amp \"one\"", "Ampli"]);
        assert_eq!(g.object(&amp, &format!("{LV2}binary")).unwrap().as_iri(), Some("file:///b/amp.lv2/amp.so"));
        assert!(g.has(&amp, &format!("{LV2}optionalFeature"), &format!("{LV2}hardRTCapable")));
        let list = g.list(g.object(&amp, "http://example.org/ns#list").unwrap());
        assert_eq!(list.iter().map(|t| t.as_f64().unwrap()).collect::<Vec<_>>(), [1.0, 2.5, -300.0]);
        assert_eq!(g.object(&amp, "http://example.org/ns#flag").unwrap().as_f64(), Some(1.0));
        assert_eq!(g.object(&amp, "http://example.org/ns#long").unwrap().as_str(), Some("two\nlines with \"quotes\" "));
        match g.object(&amp, "http://example.org/ns#typed").unwrap() {
            Term::Literal { value, datatype, .. } => {
                assert_eq!((value.as_str(), datatype.as_str()), ("7", "http://www.w3.org/2001/XMLSchema#int"))
            }
            t => panic!("{t:?}"),
        }
        let ports: Vec<_> = g.objects(&amp, &format!("{LV2}port")).cloned().collect();
        assert_eq!(ports.len(), 2);
        let gain = &ports[0];
        assert_eq!(g.object(gain, &format!("{LV2}symbol")).unwrap().as_str(), Some("gain"));
        assert_eq!(g.object(gain, &format!("{LV2}name")).unwrap().as_str(), Some("Gain"));
        assert_eq!(g.object(gain, &format!("{LV2}maximum")).unwrap().as_f64(), Some(2.0));
        let sp = g.object(gain, &format!("{LV2}scalePoint")).unwrap();
        assert_eq!(g.object(sp, "http://www.w3.org/2000/01/rdf-schema#label").unwrap().as_str(), Some("Unity"));
        assert!(g.has_type(&ports[1], &format!("{LV2}AudioPort")));
        assert_eq!(g.object(&ports[1], &format!("{LV2}index")).unwrap().as_f64(), Some(1.0));
        assert_eq!(g.subjects(RDF_TYPE, &Term::iri(&format!("{LV2}Feature"))).len(), 1);
    }

    #[test]
    fn names_with_dots_and_statement_ends() {
        let ttl = "@prefix p: <urn:p#> .\np:a.b p:c p:d.\np:e p:f 1.\np:g p:h p:i .";
        let mut g = Graph::new();
        g.parse(ttl, "urn:x").unwrap();
        assert!(g.has(&Term::iri("urn:p#a.b"), "urn:p#c", "urn:p#d"));
        assert_eq!(g.object(&Term::iri("urn:p#e"), "urn:p#f").unwrap().as_f64(), Some(1.0));
        assert!(g.has(&Term::iri("urn:p#g"), "urn:p#h", "urn:p#i"));
    }

    #[test]
    fn errors_keep_what_came_before() {
        let mut g = Graph::new();
        let e = g.parse("@prefix p: <urn:p#> .\np:a p:b p:c .\np:d q:e p:f .", "urn:x").unwrap_err();
        assert!(e.contains("line 3") && e.contains("q:"), "{e}");
        assert_eq!(g.len(), 1);
        assert!(Graph::new().parse("<urn:a> <urn:b> \"open", "urn:x").is_err());
    }
}
