//! Theme files: a small subset of CSS, read without any dependency.
//!
//! ```css
//! /* My theme */
//! :root {
//!   --name: "Midnight";
//!   --base: ponzik;           /* unspecified tokens come from a built-in */
//!   --accent: #22c55e;
//!   --bg: rgb(10 14 12);
//!   --radius-md: 10px;
//! }
//! .playlist { --lane-alt: #121816; }
//! ```
//!
//! Only custom properties are read. Values: hex colors, `rgb()`/`rgba()`,
//! `hsl()`/`hsla()`, `color-mix(in srgb, a 30%, b)`, a few color names, numbers
//! with `px` or `%`, density keywords and `var(--x)` references (to any
//! declared property, or to the base theme's tokens). Scoped blocks (`.rack`,
//! `.piano`, `.playlist`, `.mixer`, `.devices`, `.browser`, `.topbar`) override
//! tokens in that area only. Errors carry their line number and never stop the
//! rest of the file from applying.

use ponzik_i18n::{tr, tr_fmt};
use ponzik_ui::Color;

use super::theme::{DENSITY_COMFORTABLE, DENSITY_COMPACT, Kind, TOKENS, Theme, Val, hex_string, token_index};

/// Areas of the interface a scoped block can restyle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Area {
    Rack,
    Piano,
    Playlist,
    Mixer,
    Devices,
    Browser,
    Topbar,
}

impl Area {
    pub const ALL: [Area; 7] = [Area::Rack, Area::Piano, Area::Playlist, Area::Mixer, Area::Devices, Area::Browser, Area::Topbar];

    pub fn selector(self) -> &'static str {
        match self {
            Area::Rack => "rack",
            Area::Piano => "piano",
            Area::Playlist => "playlist",
            Area::Mixer => "mixer",
            Area::Devices => "devices",
            Area::Browser => "browser",
            Area::Topbar => "topbar",
        }
    }

    pub fn index(self) -> usize {
        Area::ALL.iter().position(|a| *a == self).unwrap_or(0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Root,
    Area(Area),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CssError {
    pub line: usize,
    pub msg: String,
}

impl std::fmt::Display for CssError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "line {}: {}", self.line, self.msg)
    }
}

#[derive(Clone, Debug)]
pub struct Decl {
    /// Without the leading `--`.
    pub name: String,
    pub value: String,
    pub line: usize,
    /// Bytes of the whole declaration (name to `;` included) and of its value.
    pub span: (usize, usize),
    pub value_span: (usize, usize),
}

#[derive(Clone, Debug)]
pub struct Block {
    pub scopes: Vec<Scope>,
    pub decls: Vec<Decl>,
    /// Byte offset of the closing `}` (the end of the text if missing).
    pub close: usize,
}

#[derive(Clone, Debug, Default)]
pub struct Sheet {
    pub blocks: Vec<Block>,
    pub errors: Vec<CssError>,
}

fn line_at(src: &str, at: usize) -> usize {
    src.as_bytes()[..at.min(src.len())].iter().filter(|&&b| b == b'\n').count() + 1
}

/// Blanks comments out (same length, newlines kept) so offsets and line
/// numbers stay those of the original text.
fn strip_comments(src: &str, errors: &mut Vec<CssError>) -> String {
    let b = src.as_bytes();
    let mut out = b.to_vec();
    let mut i = 0;
    let mut quote: Option<u8> = None;
    while i < b.len() {
        match quote {
            Some(q) => {
                if b[i] == q {
                    quote = None;
                }
                i += 1;
            }
            None if b[i] == b'"' || b[i] == b'\'' => {
                quote = Some(b[i]);
                i += 1;
            }
            None if b[i] == b'/' && b.get(i + 1) == Some(&b'*') => {
                let start = i;
                let end = src[i + 2..].find("*/").map(|k| i + 2 + k + 2);
                if end.is_none() {
                    errors.push(CssError { line: line_at(src, start), msg: tr("comment never closed (missing */)").into() });
                }
                let end = end.unwrap_or(b.len());
                for o in &mut out[start..end] {
                    if *o != b'\n' {
                        *o = b' ';
                    }
                }
                i = end;
            }
            None => i += 1,
        }
    }
    // Only ASCII bytes were replaced, by spaces: still valid UTF-8 except inside
    // multibyte characters of comments, which are blanked byte by byte too.
    String::from_utf8(out).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

/// Splits a theme file into blocks and declarations. Never fails: problems are
/// listed in [`Sheet::errors`] and the rest is kept.
pub fn parse(src: &str) -> Sheet {
    let mut sheet = Sheet::default();
    let clean = strip_comments(src, &mut sheet.errors);
    let b = clean.as_bytes();
    let mut i = 0;
    while i < b.len() {
        // Selector up to `{`.
        let Some(open) = clean[i..].find('{').map(|k| i + k) else {
            let rest = clean[i..].trim();
            if !rest.is_empty() {
                let at = i + clean[i..].find(|c: char| !c.is_whitespace()).unwrap_or(0);
                sheet.errors.push(CssError { line: line_at(src, at), msg: tr_fmt!("expected a block like \":root {{ … }}\", found \"{}\"", short(rest)) });
            }
            break;
        };
        let sel_text = clean[i..open].trim();
        let sel_line = line_at(src, i + clean[i..open].find(|c: char| !c.is_whitespace()).unwrap_or(0));
        if sel_text.contains('}') {
            sheet.errors.push(CssError { line: sel_line, msg: tr("unexpected \"}\"").into() });
        }
        let sel_text = sel_text.rsplit('}').next().unwrap_or("").trim();
        let mut scopes = Vec::new();
        let mut bad_selector = false;
        for part in sel_text.split(',').map(str::trim) {
            match selector(part) {
                Some(s) => scopes.push(s),
                None => {
                    bad_selector = true;
                    let known: Vec<String> = Area::ALL.iter().map(|a| format!(".{}", a.selector())).collect();
                    sheet.errors.push(CssError {
                        line: sel_line,
                        msg: tr_fmt!("unknown selector \"{}\" (use :root or one of {})", short(part), known.join(", ")),
                    });
                }
            }
        }
        // Body up to the matching `}`.
        let close = match clean[open + 1..].find(['}', '{']).map(|k| open + 1 + k) {
            Some(k) if b[k] == b'}' => k,
            Some(k) => {
                sheet.errors.push(CssError { line: line_at(src, k), msg: tr("nested blocks are not supported").into() });
                // Skip to the end of the nested block and the outer one.
                let after = clean[k..].find('}').map_or(b.len(), |e| k + e + 1);
                i = clean[after..].find('}').map_or(b.len(), |e| after + e + 1);
                continue;
            }
            None => {
                sheet.errors.push(CssError { line: line_at(src, open), msg: tr("block never closed (missing })").into() });
                b.len()
            }
        };
        let decls = declarations(src, &clean, open + 1, close, &mut sheet.errors);
        if !bad_selector || !scopes.is_empty() {
            sheet.blocks.push(Block { scopes, decls, close });
        }
        i = close + 1;
    }
    sheet
}

fn short(s: &str) -> String {
    let s = s.trim();
    if s.chars().count() > 40 { format!("{}…", s.chars().take(40).collect::<String>()) } else { s.to_string() }
}

fn selector(s: &str) -> Option<Scope> {
    if s == ":root" || s == "*" || s == "html" {
        return Some(Scope::Root);
    }
    let name = s.strip_prefix('.')?;
    Area::ALL.iter().find(|a| a.selector() == name).map(|&a| Scope::Area(a))
}

fn declarations(src: &str, clean: &str, from: usize, to: usize, errors: &mut Vec<CssError>) -> Vec<Decl> {
    let mut out = Vec::new();
    let mut i = from;
    while i < to {
        let body = &clean[i..to];
        let Some(start) = body.find(|c: char| !c.is_whitespace() && c != ';').map(|k| i + k) else { break };
        // A declaration ends at `;` outside quotes and parentheses, or at the block end.
        let mut end = to;
        let mut depth = 0i32;
        let mut quote: Option<u8> = None;
        for (k, &ch) in clean.as_bytes()[start..to].iter().enumerate() {
            match (quote, ch) {
                (Some(q), c) if c == q => quote = None,
                (Some(_), _) => {}
                (None, b'"' | b'\'') => quote = Some(ch),
                (None, b'(') => depth += 1,
                (None, b')') => depth -= 1,
                (None, b';') if depth <= 0 => {
                    end = start + k;
                    break;
                }
                _ => {}
            }
        }
        let text = &clean[start..end];
        let line = line_at(src, start);
        let span_end = if end < to { end + 1 } else { start + text.trim_end().len() };
        i = span_end.max(start + 1);
        let Some(colon) = text.find(':') else {
            errors.push(CssError { line, msg: tr_fmt!("expected \"--name: value;\", found \"{}\"", short(text)) });
            continue;
        };
        let name = text[..colon].trim();
        let raw_value = &text[colon + 1..];
        let lead = raw_value.len() - raw_value.trim_start().len();
        let value = raw_value.trim();
        let value = value.strip_suffix("!important").map_or(value, str::trim_end);
        let Some(name) = name.strip_prefix("--") else {
            errors.push(CssError { line, msg: tr_fmt!("\"{}\" is not a theme property: names start with \"--\", like --accent", name) });
            continue;
        };
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            errors.push(CssError { line, msg: tr_fmt!("invalid property name \"--{}\"", name) });
            continue;
        }
        if value.is_empty() {
            errors.push(CssError { line, msg: tr_fmt!("--{} has no value", name) });
            continue;
        }
        let vstart = start + colon + 1 + lead;
        out.push(Decl { name: name.to_string(), value: value.to_string(), line, span: (start, span_end), value_span: (vstart, vstart + value.len()) });
    }
    out
}

// --- Values ---

const NAMED: [(&str, u32); 12] = [
    ("black", 0x000000),
    ("white", 0xffffff),
    ("red", 0xff0000),
    ("green", 0x008000),
    ("lime", 0x00ff00),
    ("blue", 0x0000ff),
    ("yellow", 0xffff00),
    ("cyan", 0x00ffff),
    ("magenta", 0xff00ff),
    ("orange", 0xffa500),
    ("gray", 0x808080),
    ("grey", 0x808080),
];

/// Reads a color: `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa`, `rgb()`, `rgba()`,
/// `hsl()`, `hsla()`, `color-mix(in srgb, a p%, b)`, a few names, `transparent`.
pub fn parse_color(s: &str) -> Result<Color, String> {
    let s = s.trim();
    let lower = s.to_ascii_lowercase();
    if let Some(h) = lower.strip_prefix('#') {
        return parse_hex(h).ok_or_else(|| tr_fmt!("\"{}\" is not a hex color (#rgb, #rrggbb or #rrggbbaa)", s));
    }
    if lower == "transparent" {
        return Ok(Color::TRANSPARENT);
    }
    if let Some(&(_, hex)) = NAMED.iter().find(|(n, _)| *n == lower) {
        return Ok(Color::hex(hex));
    }
    let Some(open) = lower.find('(') else { return Err(tr_fmt!("\"{}\" is not a color", s)) };
    if !lower.ends_with(')') {
        return Err(tr_fmt!("missing \")\" in \"{}\"", s));
    }
    let func = lower[..open].trim();
    let args = &lower[open + 1..lower.len() - 1];
    match func {
        "rgb" | "rgba" => {
            let (parts, alpha) = components(args)?;
            if parts.len() != 3 {
                return Err(tr_fmt!("{}() needs 3 components and an optional alpha, in \"{}\"", func, s));
            }
            let ch = |p: &str| -> Result<u8, String> {
                let v = if let Some(pc) = p.strip_suffix('%') { num(pc)? * 2.55 } else { num(p)? };
                Ok(v.round().clamp(0.0, 255.0) as u8)
            };
            Ok(Color::rgba(ch(&parts[0])?, ch(&parts[1])?, ch(&parts[2])?, alpha_of(alpha.as_deref())?))
        }
        "hsl" | "hsla" => {
            let (parts, alpha) = components(args)?;
            if parts.len() != 3 {
                return Err(tr_fmt!("{}() needs hue, saturation and lightness, in \"{}\"", func, s));
            }
            let h = angle(&parts[0])?;
            let pc = |p: &str| -> Result<f32, String> { Ok(num(p.trim_end_matches('%'))? / 100.0) };
            Ok(super::theme::from_hsl(h, pc(&parts[1])?, pc(&parts[2])?, alpha_of(alpha.as_deref())?))
        }
        "color-mix" => color_mix(args, s),
        _ => Err(tr_fmt!("unknown color function \"{}()\"", func)),
    }
}

fn parse_hex(h: &str) -> Option<Color> {
    if !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let d = |i: usize| u8::from_str_radix(&h[i..i + 1], 16).ok().map(|v| v * 17);
    let dd = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    match h.len() {
        3 => Some(Color::rgba(d(0)?, d(1)?, d(2)?, 255)),
        4 => Some(Color::rgba(d(0)?, d(1)?, d(2)?, d(3)?)),
        6 => Some(Color::rgba(dd(0)?, dd(2)?, dd(4)?, 255)),
        8 => Some(Color::rgba(dd(0)?, dd(2)?, dd(4)?, dd(6)?)),
        _ => None,
    }
}

fn num(s: &str) -> Result<f32, String> {
    let s = s.trim();
    s.parse::<f32>().ok().filter(|v| v.is_finite()).ok_or_else(|| tr_fmt!("\"{}\" is not a number", s))
}

fn angle(s: &str) -> Result<f32, String> {
    let s = s.trim();
    if let Some(v) = s.strip_suffix("deg") {
        num(v)
    } else if let Some(v) = s.strip_suffix("turn") {
        Ok(num(v)? * 360.0)
    } else if let Some(v) = s.strip_suffix("rad") {
        Ok(num(v)?.to_degrees())
    } else {
        num(s)
    }
}

fn alpha_of(a: Option<&str>) -> Result<u8, String> {
    let Some(a) = a else { return Ok(255) };
    let v = if let Some(pc) = a.strip_suffix('%') { num(pc)? / 100.0 } else { num(a)? };
    Ok((v.clamp(0.0, 1.0) * 255.0).round() as u8)
}

/// Components of `rgb(1, 2, 3, 0.5)` or `rgb(1 2 3 / 50%)`: three values and an alpha.
fn components(args: &str) -> Result<(Vec<String>, Option<String>), String> {
    if args.contains(',') {
        let mut parts: Vec<String> = args.split(',').map(|p| p.trim().to_string()).collect();
        if parts.iter().any(String::is_empty) {
            return Err(tr_fmt!("empty component in \"{}\"", args));
        }
        let alpha = (parts.len() == 4).then(|| parts.pop()).flatten();
        return Ok((parts, alpha));
    }
    let (main, alpha) = match args.split_once('/') {
        Some((m, a)) => (m, Some(a.trim().to_string())),
        None => (args, None),
    };
    Ok((main.split_whitespace().map(str::to_string).collect(), alpha))
}

/// Splits on commas outside parentheses.
fn top_level_commas(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut depth, mut start) = (0i32, 0);
    for (i, ch) in s.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out
}

fn color_mix(args: &str, whole: &str) -> Result<Color, String> {
    let parts = top_level_commas(args);
    if parts.len() != 3 || !parts[0].trim().starts_with("in ") {
        return Err(tr_fmt!("expected color-mix(in srgb, color [p%], color [q%]), found \"{}\"", whole));
    }
    let space = parts[0].trim()[3..].trim();
    if space != "srgb" {
        return Err(tr_fmt!("color-mix: only \"in srgb\" is supported, not \"{}\"", space));
    }
    let item = |p: &str| -> Result<(Color, Option<f32>), String> {
        let p = p.trim();
        // The percentage, if any, is the last word outside parentheses.
        match p.rsplit_once(' ').filter(|(c, pc)| pc.ends_with('%') && !c.trim().is_empty() && c.matches('(').count() == c.matches(')').count()) {
            Some((c, pc)) => Ok((parse_color(c)?, Some(num(pc.trim_end_matches('%'))? / 100.0))),
            None => Ok((parse_color(p)?, None)),
        }
    };
    let (a, pa) = item(parts[1])?;
    let (b, pb) = item(parts[2])?;
    let wa = match (pa, pb) {
        (Some(x), Some(y)) if x + y > 0.0 => x / (x + y),
        (Some(x), _) => x,
        (None, Some(y)) => 1.0 - y,
        (None, None) => 0.5,
    };
    Ok(b.mix(a, wa.clamp(0.0, 1.0)))
}

/// Reads a numeric token value of the given kind.
pub fn parse_number(s: &str, kind: Kind) -> Result<f32, String> {
    let s = s.trim().to_ascii_lowercase();
    match kind {
        Kind::Density => match s.as_str() {
            "compact" => return Ok(DENSITY_COMPACT),
            "normal" | "default" => return Ok(1.0),
            "comfortable" | "spacious" => return Ok(DENSITY_COMFORTABLE),
            _ => {}
        },
        Kind::Factor if s == "none" => return Ok(0.0),
        Kind::Px => {
            return if let Some(v) = s.strip_suffix("px") {
                num(v)
            } else if s.ends_with("em") || s.ends_with('%') {
                Err(tr_fmt!("\"{}\": lengths are in px", s.as_str()))
            } else {
                num(&s)
            };
        }
        _ => {}
    }
    if let Some(pc) = s.strip_suffix('%') {
        return Ok(num(pc)? / 100.0);
    }
    num(&s).map_err(|_| match kind {
        Kind::Density => tr_fmt!("\"{}\": use compact, normal, comfortable or a number", s.as_str()),
        _ => tr_fmt!("\"{}\" is not a number", s.as_str()),
    })
}

/// Text of a token value, as written in a theme file.
pub fn value_text(def_kind: Kind, v: Val) -> String {
    match v {
        Val::Color(c) => hex_string(c),
        Val::Num(x) => {
            if def_kind == Kind::Density {
                if x == DENSITY_COMPACT {
                    return "compact".into();
                } else if x == 1.0 {
                    return "normal".into();
                } else if x == DENSITY_COMFORTABLE {
                    return "comfortable".into();
                }
            }
            let n = if x.fract() == 0.0 { format!("{x:.0}") } else { format!("{}", (x * 1000.0).round() / 1000.0) };
            if def_kind == Kind::Px { format!("{n}px") } else { n }
        }
    }
}

/// A token's value read from text, checked against its kind.
pub fn parse_token_value(index: usize, s: &str) -> Result<Val, String> {
    let def = &TOKENS[index];
    match def.kind {
        Kind::Color => parse_color(s).map(Val::Color),
        k => {
            let v = parse_number(s, k)?;
            let (lo, hi) = def.range;
            if v < lo || v > hi {
                return Err(tr_fmt!("--{} must be between {} and {}", def.name, value_text(k, Val::Num(lo)), value_text(k, Val::Num(hi))));
            }
            Ok(Val::Num(v))
        }
    }
}

// --- Compilation ---

/// What a theme inherits: a built-in's values and, for a theme file, its own
/// overrides on top.
#[derive(Clone, Debug)]
pub struct Base {
    pub raw: Theme,
    pub overrides: Vec<(usize, Val)>,
}

impl Base {
    pub fn theme(&self) -> Theme {
        Theme::resolve(&self.raw, &self.overrides)
    }
}

/// A theme file turned into token overrides.
#[derive(Clone, Debug, Default)]
pub struct Compiled {
    pub name: Option<String>,
    pub base: Option<String>,
    pub root: Vec<(usize, Val)>,
    pub areas: Vec<(Area, Vec<(usize, Val)>)>,
    pub errors: Vec<CssError>,
}

const META: [&str; 4] = ["name", "base", "author", "description"];

/// Reads the `--base` of a sheet (first one in `:root`), to look up the parent first.
pub fn base_name(sheet: &Sheet) -> Option<String> {
    root_decls(sheet).find(|d| d.name == "base").map(|d| unquote(&d.value).to_ascii_lowercase())
}

fn root_decls(sheet: &Sheet) -> impl Iterator<Item = &Decl> {
    sheet.blocks.iter().filter(|b| b.scopes.contains(&Scope::Root)).flat_map(|b| b.decls.iter())
}

pub fn unquote(s: &str) -> String {
    let s = s.trim();
    let q = |c: char| s.len() >= 2 && s.starts_with(c) && s.ends_with(c);
    if q('"') || q('\'') { s[1..s.len() - 1].to_string() } else { s.to_string() }
}

/// Turns a parsed sheet into overrides, `base` giving the values that
/// `var(--x)` falls back to when the file does not declare `--x`.
pub fn compile(sheet: &Sheet, base: &Base) -> Compiled {
    let mut out = Compiled { errors: sheet.errors.clone(), ..Default::default() };
    let base_theme = base.theme();
    let root: Vec<&Decl> = root_decls(sheet).collect();
    // Every name referenced by a var(): such properties are variables, not unknown tokens.
    let mut referenced: Vec<String> = Vec::new();
    for b in &sheet.blocks {
        for d in &b.decls {
            let mut rest = d.value.as_str();
            while let Some(k) = rest.find("var(") {
                rest = &rest[k + 4..];
                let name = rest.trim_start().trim_start_matches("--");
                let end = name.find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_')).unwrap_or(name.len());
                referenced.push(name[..end].to_string());
            }
        }
    }
    for d in &root {
        match d.name.as_str() {
            "name" => out.name = Some(unquote(&d.value)),
            "base" => out.base = Some(unquote(&d.value).to_ascii_lowercase()),
            _ => {}
        }
    }
    let scopes: Vec<Option<Area>> = std::iter::once(None).chain(Area::ALL.iter().map(|&a| Some(a))).collect();
    for scope in scopes {
        let local: Vec<&Decl> = match scope {
            None => root.clone(),
            Some(a) => sheet.blocks.iter().filter(|b| b.scopes.contains(&Scope::Area(a))).flat_map(|b| b.decls.iter()).collect(),
        };
        if scope.is_some() && local.is_empty() {
            continue;
        }
        let env = Env { local: &local, root: &root, base: &base_theme };
        let mut list = Vec::new();
        for d in &local {
            if META.contains(&d.name.as_str()) {
                if scope.is_some() {
                    out.errors.push(CssError { line: d.line, msg: tr_fmt!("--{} belongs in :root", d.name.as_str()) });
                }
                continue;
            }
            let Some(index) = token_index(&d.name) else {
                if !referenced.contains(&d.name) {
                    let hint = closest_token(&d.name).map_or(String::new(), |n| tr_fmt!(" (did you mean --{}?)", n));
                    out.errors.push(CssError { line: d.line, msg: tr_fmt!("unknown token --{}{}", d.name.as_str(), hint) });
                }
                continue;
            };
            let value = match env.substitute(&d.value, &mut vec![d.name.clone()]) {
                Ok(v) => v,
                Err(e) => {
                    out.errors.push(CssError { line: d.line, msg: e });
                    continue;
                }
            };
            match parse_token_value(index, &value) {
                Ok(v) => {
                    list.retain(|(i, _)| *i != index);
                    list.push((index, v));
                }
                Err(e) => out.errors.push(CssError { line: d.line, msg: format!("--{}: {e}", d.name) }),
            }
        }
        match scope {
            None => out.root = list,
            Some(a) => out.areas.push((a, list)),
        }
    }
    out.errors.sort_by_key(|e| e.line);
    out
}

/// Nearest token name, for typos (`--acent` → `--accent`).
fn closest_token(name: &str) -> Option<&'static str> {
    let dist = |a: &str, b: &str| {
        let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
        let mut row: Vec<usize> = (0..=b.len()).collect();
        for i in 1..=a.len() {
            let mut prev = row[0];
            row[0] = i;
            for j in 1..=b.len() {
                let cur = row[j];
                row[j] = (row[j] + 1).min(row[j - 1] + 1).min(prev + usize::from(a[i - 1] != b[j - 1]));
                prev = cur;
            }
        }
        row[b.len()]
    };
    TOKENS.iter().map(|t| (dist(name, t.name), t.name)).filter(|(d, _)| *d <= 2).min_by_key(|(d, _)| *d).map(|(_, n)| n)
}

struct Env<'a> {
    local: &'a [&'a Decl],
    root: &'a [&'a Decl],
    base: &'a Theme,
}

impl Env<'_> {
    /// Replaces every `var(--x[, fallback])` by the value of `--x`.
    fn substitute(&self, value: &str, stack: &mut Vec<String>) -> Result<String, String> {
        let Some(k) = value.find("var(") else { return Ok(value.to_string()) };
        let open = k + 4;
        let mut depth = 1;
        let mut close = None;
        for (i, ch) in value[open..].char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(open + i);
                        break;
                    }
                }
                _ => {}
            }
        }
        let close = close.ok_or_else(|| tr_fmt!("missing \")\" after var( in \"{}\"", short(value)))?;
        let inner = &value[open..close];
        let (name, fallback) = match top_level_commas(inner).as_slice() {
            [n] => (n.trim(), None),
            [n, rest @ ..] => (n.trim(), Some(rest.join(",").trim().to_string())),
            [] => ("", None),
        };
        let name = name.strip_prefix("--").ok_or_else(|| tr_fmt!("var() takes a --name, found \"{}\"", name))?;
        if stack.iter().any(|s| s == name) {
            return Err(tr_fmt!("var(--{}) refers to itself", name));
        }
        if stack.len() > 16 {
            return Err(tr("var() references nested too deeply").into());
        }
        let found = self.local.iter().rev().chain(self.root.iter().rev()).find(|d| d.name == name);
        let resolved = if let Some(d) = found {
            stack.push(name.to_string());
            let v = self.substitute(&d.value, stack);
            stack.pop();
            v?
        } else if let Some(i) = token_index(name) {
            value_text(TOKENS[i].kind, self.base.get(i))
        } else if let Some(fb) = fallback {
            self.substitute(&fb, stack)?
        } else {
            return Err(tr_fmt!("var(--{}): --{} is not declared", name, name));
        };
        let rest = self.substitute(&value[close + 1..], stack)?;
        Ok(format!("{}{}{}", &value[..k], resolved, rest))
    }
}

// --- Writing ---

/// A complete theme file: name, base, then every token that differs from the base.
pub fn export(name: &str, base_id: &str, base: &Theme, theme: &Theme, areas: &[(Area, Theme)]) -> String {
    let mut s = format!("/* {} : Ponzik theme */\n:root {{\n  --name: \"{}\";\n  --base: {};\n", name.replace("*/", ""), name.replace('"', "'"), base_id.to_ascii_lowercase());
    for (i, def) in TOKENS.iter().enumerate() {
        if theme.get(i) != base.get(i) {
            s.push_str(&format!("  --{}: {};\n", def.name, value_text(def.kind, theme.get(i))));
        }
    }
    s.push_str("}\n");
    for (area, t) in areas {
        let lines: Vec<String> = TOKENS
            .iter()
            .enumerate()
            .filter(|(i, _)| t.get(*i) != theme.get(*i))
            .map(|(i, def)| format!("  --{}: {};\n", def.name, value_text(def.kind, t.get(i))))
            .collect();
        if !lines.is_empty() {
            s.push_str(&format!("\n.{} {{\n{}}}\n", area.selector(), lines.concat()));
        }
    }
    s
}

/// First `:root`-only block, where the editor writes.
fn root_block(sheet: &Sheet) -> Option<&Block> {
    sheet.blocks.iter().find(|b| b.scopes == [Scope::Root])
}

/// Sets `--name: value` in the `:root` block, in place: comments and the rest of
/// the file are kept.
pub fn set_root_decl(src: &str, name: &str, value: &str) -> String {
    let sheet = parse(src);
    let Some(block) = root_block(&sheet) else {
        let sep = if src.is_empty() || src.ends_with('\n') { "" } else { "\n" };
        return format!("{src}{sep}:root {{\n  --{name}: {value};\n}}\n");
    };
    if let Some(d) = block.decls.iter().rev().find(|d| d.name == name) {
        let mut s = src.to_string();
        // A declaration without `;` at the end of the block gets one.
        let tail = if src[d.value_span.1..d.span.1].contains(';') { "" } else { ";" };
        s.replace_range(d.value_span.0..d.value_span.1, &format!("{value}{tail}"));
        return s;
    }
    // New declaration on its own line, before the closing brace.
    let close = block.close.min(src.len());
    let line_start = src[..close].rfind('\n').map_or(0, |k| k + 1);
    let only_space = src[line_start..close].trim().is_empty();
    let needs_semicolon = block.decls.last().is_some_and(|d| !src[d.value_span.1..d.span.1].contains(';'));
    let mut s = src.to_string();
    if only_space {
        s.insert_str(line_start, &format!("  --{name}: {value};\n"));
    } else {
        s.insert_str(close, &format!("\n  --{name}: {value};\n"));
    }
    if needs_semicolon && let Some(d) = block.decls.last() {
        s.insert(d.value_span.1, ';');
    }
    s
}

/// Removes every `--name` declaration of the `:root` block (and its line if it
/// held nothing else).
pub fn remove_root_decl(src: &str, name: &str) -> String {
    let sheet = parse(src);
    let Some(block) = root_block(&sheet) else { return src.to_string() };
    let mut s = src.to_string();
    for d in block.decls.iter().rev().filter(|d| d.name == name) {
        let (mut a, mut b) = d.span;
        let line_start = s[..a].rfind('\n').map_or(0, |k| k + 1);
        let line_end = s[b..].find('\n').map_or(s.len(), |k| b + k);
        if s[line_start..a].trim().is_empty() && s[b..line_end].trim().is_empty() {
            a = line_start;
            b = (line_end + 1).min(s.len());
        }
        s.replace_range(a..b, "");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::super::theme::{BUILTINS, PONZIK_RAW, ponzik};
    use super::*;

    fn base() -> Base {
        Base { raw: PONZIK_RAW, overrides: Vec::new() }
    }

    fn compile_str(src: &str) -> Compiled {
        compile(&parse(src), &base())
    }

    fn color_of(c: &Compiled, name: &str) -> Option<Color> {
        let i = token_index(name)?;
        c.root.iter().find(|(k, _)| *k == i).and_then(|(_, v)| if let Val::Color(c) = v { Some(*c) } else { None })
    }

    #[test]
    fn colors() {
        let ok = |s: &str| parse_color(s).unwrap();
        assert_eq!(ok("#22c55e"), Color::hex(0x22c55e));
        assert_eq!(ok("#ABC"), Color::hex(0xaabbcc));
        assert_eq!(ok("#abcd"), Color::rgba(0xaa, 0xbb, 0xcc, 0xdd));
        assert_eq!(ok("#11223380"), Color::rgba(0x11, 0x22, 0x33, 0x80));
        assert_eq!(ok("rgb(10 14 12)"), Color::hex(0x0a0e0c));
        assert_eq!(ok("rgb(10, 14, 12)"), Color::hex(0x0a0e0c));
        assert_eq!(ok("rgba(255, 0, 0, 0.5)"), Color::rgba(255, 0, 0, 128));
        assert_eq!(ok("rgb(100% 0% 0% / 50%)"), Color::rgba(255, 0, 0, 128));
        assert_eq!(ok("hsl(0 100% 50%)"), Color::hex(0xff0000));
        assert_eq!(ok("hsl(120deg, 100%, 25%)"), Color::hex(0x008000));
        assert_eq!(ok("hsla(240 100% 50% / 0.5)"), Color::rgba(0, 0, 255, 128));
        assert_eq!(ok("hsl(0.5turn 100% 50%)"), Color::hex(0x00ffff));
        assert_eq!(ok("white"), Color::hex(0xffffff));
        assert_eq!(ok("transparent"), Color::TRANSPARENT);
        assert_eq!(ok("color-mix(in srgb, #ffffff 25%, #000000)"), Color::hex(0x404040));
        assert_eq!(ok("color-mix(in srgb, white, black)"), Color::hex(0x808080));
        assert_eq!(ok("color-mix(in srgb, rgb(255 0 0), #0000ff 75%)"), Color::rgba(64, 0, 191, 255));
        for bad in ["#12", "#ggg", "rgb(1 2)", "hsl(1 2 3", "foo", "lab(50% 0 0)", "color-mix(in oklab, red, blue)"] {
            assert!(parse_color(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn numbers() {
        assert_eq!(parse_number("10px", Kind::Px), Ok(10.0));
        assert_eq!(parse_number("6", Kind::Px), Ok(6.0));
        assert!(parse_number("1em", Kind::Px).is_err());
        assert_eq!(parse_number("110%", Kind::Factor), Ok(1.1));
        assert_eq!(parse_number("compact", Kind::Density), Ok(DENSITY_COMPACT));
        assert_eq!(parse_number("none", Kind::Factor), Ok(0.0));
        assert!(parse_token_value(token_index("font-scale").unwrap(), "3").is_err());
    }

    #[test]
    fn sheet_with_comments_vars_and_scopes() {
        let src = "/* My theme\n   spans lines */\n:root {\n  --name: \"Midnight\";\n  --base: Glace; /* inline */\n  --brand: #22c55e;\n  --accent: var(--brand);\n  --select: var(--undeclared, #ff0000);\n  --raised: color-mix(in srgb, var(--bg) 50%, white);\n  --radius-md: 10px;\n  --font-scale: 1.1;\n  --density: compact\n}\n.playlist { --lane-alt: #121816; }\n.mixer, .rack { --meter-hot: hsl(0 80% 60%); }\n";
        let c = compile_str(src);
        assert!(c.errors.is_empty(), "{:?}", c.errors);
        assert_eq!(c.name.as_deref(), Some("Midnight"));
        assert_eq!(c.base.as_deref(), Some("glace"));
        assert_eq!(color_of(&c, "accent"), Some(Color::hex(0x22c55e)));
        assert_eq!(color_of(&c, "select"), Some(Color::hex(0xff0000)));
        // var(--bg) falls back to the base theme's value.
        assert_eq!(color_of(&c, "raised"), Some(ponzik().bg.mix(Color::hex(0xffffff), 0.5)));
        let t = Theme::resolve(&PONZIK_RAW, &c.root);
        assert_eq!((t.radius[1], t.font_scale, t.density), (10.0, 1.1, DENSITY_COMPACT));
        assert_eq!(c.areas.len(), 3);
        let pl = &c.areas.iter().find(|(a, _)| *a == Area::Playlist).unwrap().1;
        assert_eq!(pl, &vec![(token_index("lane-alt").unwrap(), Val::Color(Color::hex(0x121816)))]);
        assert!(c.areas.iter().any(|(a, v)| *a == Area::Rack && v.len() == 1));
    }

    #[test]
    fn errors_have_line_numbers_and_the_rest_applies() {
        let src = ":root {\n  --accent: #22c55e;\n  --bg: #12;\n  color: red;\n  --acent: #fff;\n  --select: var(--select);\n  --text: var(--nope);\n  --font-scale: big;\n}\n.sidebar { --bg: #000; }\n.rack { --x: 1 { }\n/* never closed\n";
        let c = compile_str(src);
        let lines: Vec<usize> = c.errors.iter().map(|e| e.line).collect();
        for l in [3, 4, 5, 6, 7, 8, 10, 11, 12] {
            assert!(lines.contains(&l), "line {l} missing in {:?}", c.errors);
        }
        assert!(c.errors.iter().any(|e| e.msg.contains("did you mean --accent")), "{:?}", c.errors);
        assert_eq!(color_of(&c, "accent"), Some(Color::hex(0x22c55e)));
        assert!(color_of(&c, "bg").is_none());
        // Garbage never panics.
        for junk in ["", "}", "{{{", ":root", ":root {", "--a: b;", ":root { --accent: rgb(", "\u{0}\u{ffff}é{", "/*", ":root{--a:var(--b);--b:var(--a);--accent:var(--a)}"] {
            let _ = compile_str(junk);
        }
    }

    #[test]
    fn in_place_edits_keep_comments() {
        let src = "/* keep me */\n:root {\n  --accent: #111111; /* mine */\n  --bg: #000\n}\n";
        let s = set_root_decl(src, "accent", "#222222");
        assert!(s.contains("--accent: #222222; /* mine */") && s.contains("/* keep me */"), "{s}");
        let s = set_root_decl(&s, "select", "#333333");
        let c = compile_str(&s);
        assert!(c.errors.is_empty(), "{s}\n{:?}", c.errors);
        assert_eq!(color_of(&c, "select"), Some(Color::hex(0x333333)));
        assert_eq!(color_of(&c, "bg"), Some(Color::hex(0x000000)));
        let s = remove_root_decl(&s, "accent");
        assert!(!s.contains("--accent") && s.contains("/* keep me */"), "{s}");
        assert_eq!(set_root_decl("", "accent", "#fff"), ":root {\n  --accent: #fff;\n}\n");
    }

    #[test]
    fn export_import_round_trip() {
        for b in &BUILTINS {
            let base_theme = Theme::resolve(&PONZIK_RAW, &[]);
            let theme = Theme::resolve(&b.raw, &[(token_index("radius-md").unwrap(), Val::Num(11.0)), (token_index("density").unwrap(), Val::Num(DENSITY_COMFORTABLE))]);
            let mut mixer = theme;
            mixer.meter_hot = Color::rgba(1, 2, 3, 128);
            let text = export("Round \"trip\"", "ponzik", &base_theme, &theme, &[(Area::Mixer, mixer)]);
            let c = compile_str(&text);
            assert!(c.errors.is_empty(), "{text}\n{:?}", c.errors);
            assert_eq!(Theme::resolve(&PONZIK_RAW, &c.root), theme, "{text}");
            let mut both = c.root.clone();
            both.extend(c.areas[0].1.iter().copied());
            assert_eq!(Theme::resolve(&PONZIK_RAW, &both), mixer);
            assert_eq!(c.name.as_deref(), Some("Round 'trip'"));
        }
    }
}
