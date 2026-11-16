//! Mise en forme à l'exécution des textes traduits : le gabarit vient du
//! catalogue, donc `format!` ne peut pas servir. Syntaxe reprise de Rust :
//! `{}`, `{1}`, `{:.2}`, `{:>6}`, `{:+.1}`, `{:03}`, `{{` et `}}`.

use std::borrow::Cow;
use std::fmt::Write as _;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Spec {
    pub fill: Option<char>,
    pub align: Option<char>,
    pub plus: bool,
    pub zero: bool,
    pub width: usize,
    pub precision: Option<usize>,
}

/// Valeur insérable dans un texte traduit.
pub trait Arg {
    fn write(&self, spec: &Spec, out: &mut String);
}

fn pad(out: &mut String, s: &str, spec: &Spec, numeric: bool) {
    let len = s.chars().count();
    if len >= spec.width {
        out.push_str(s);
        return;
    }
    let n = spec.width - len;
    if numeric && spec.zero && spec.align.is_none() {
        let (sign, digits) = match s.chars().next() {
            Some(c @ ('-' | '+' | '−')) => (Some(c), &s[c.len_utf8()..]),
            _ => (None, s),
        };
        if let Some(c) = sign {
            out.push(c);
        }
        out.extend(std::iter::repeat_n('0', n));
        out.push_str(digits);
        return;
    }
    let fill = spec.fill.unwrap_or(' ');
    let (left, right) = match spec.align.unwrap_or(if numeric { '>' } else { '<' }) {
        '>' => (n, 0),
        '^' => (n / 2, n - n / 2),
        _ => (0, n),
    };
    out.extend(std::iter::repeat_n(fill, left));
    out.push_str(s);
    out.extend(std::iter::repeat_n(fill, right));
}

macro_rules! int_arg {
    ($($t:ty),*) => {$(
        impl Arg for $t {
            fn write(&self, spec: &Spec, out: &mut String) {
                let s = if spec.plus && *self as i128 >= 0 { format!("+{self}") } else { self.to_string() };
                pad(out, &s, spec, true);
            }
        }
    )*};
}
int_arg!(i8, i16, i32, i64, isize, u8, u16, u32, u64, usize);

macro_rules! float_arg {
    ($($t:ty),*) => {$(
        impl Arg for $t {
            fn write(&self, spec: &Spec, out: &mut String) {
                let mut s = match spec.precision {
                    Some(p) => format!("{self:.p$}"),
                    None => self.to_string(),
                };
                if spec.plus && !s.starts_with('-') {
                    s.insert(0, '+');
                }
                pad(out, &crate::localize_number(s), spec, true);
            }
        }
    )*};
}
float_arg!(f32, f64);

impl Arg for str {
    fn write(&self, spec: &Spec, out: &mut String) {
        match spec.precision {
            Some(p) => pad(out, &self.chars().take(p).collect::<String>(), spec, false),
            None => pad(out, self, spec, false),
        }
    }
}

impl Arg for String {
    fn write(&self, spec: &Spec, out: &mut String) {
        self.as_str().write(spec, out);
    }
}

impl Arg for Cow<'_, str> {
    fn write(&self, spec: &Spec, out: &mut String) {
        self.as_ref().write(spec, out);
    }
}

impl Arg for char {
    fn write(&self, spec: &Spec, out: &mut String) {
        self.encode_utf8(&mut [0; 4]).write(spec, out);
    }
}

impl<T: Arg + ?Sized> Arg for &T {
    fn write(&self, spec: &Spec, out: &mut String) {
        (**self).write(spec, out);
    }
}

impl<T: Arg + ?Sized> Arg for &mut T {
    fn write(&self, spec: &Spec, out: &mut String) {
        (**self).write(spec, out);
    }
}

/// `{index:spec}` → (index, spec), sans les accolades.
fn parse_placeholder(inner: &str) -> Option<(Option<usize>, Spec)> {
    let (idx, spec) = inner.split_once(':').unwrap_or((inner, ""));
    let idx = if idx.is_empty() { None } else { Some(idx.parse().ok()?) };
    let mut s = Spec::default();
    let mut chars: Vec<char> = spec.chars().collect();
    if chars.len() >= 2 && matches!(chars[1], '<' | '>' | '^') {
        s.fill = Some(chars[0]);
        s.align = Some(chars[1]);
        chars.drain(..2);
    } else if chars.first().is_some_and(|c| matches!(c, '<' | '>' | '^')) {
        s.align = Some(chars.remove(0));
    }
    if chars.first() == Some(&'+') {
        s.plus = true;
        chars.remove(0);
    }
    if chars.first() == Some(&'0') {
        s.zero = true;
        chars.remove(0);
    }
    let rest: String = chars.into_iter().collect();
    let (w, p) = rest.split_once('.').map_or((rest.as_str(), None), |(w, p)| (w, Some(p)));
    if !w.is_empty() {
        s.width = w.parse().ok()?;
    }
    if let Some(p) = p {
        s.precision = Some(p.parse().ok()?);
    }
    Some((idx, s))
}

/// Remplit un gabarit traduit. Un argument manquant s'affiche `{?}`.
pub fn format(template: &str, args: &[&dyn Arg]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut next = 0;
    let mut rest = template;
    while let Some(i) = rest.find(['{', '}']) {
        out.push_str(&rest[..i]);
        let c = rest.as_bytes()[i];
        let after = &rest[i + 1..];
        if after.as_bytes().first() == Some(&c) {
            out.push(c as char);
            rest = &after[1..];
            continue;
        }
        if c == b'}' {
            out.push('}');
            rest = after;
            continue;
        }
        let Some(end) = after.find('}') else {
            out.push_str(&rest[i..]);
            return out;
        };
        match parse_placeholder(&after[..end]) {
            Some((idx, spec)) => {
                let k = idx.unwrap_or_else(|| {
                    next += 1;
                    next - 1
                });
                match args.get(k) {
                    Some(a) => a.write(&spec, &mut out),
                    None => out.push_str("{?}"),
                }
            }
            None => {
                let _ = write!(out, "{{{}}}", &after[..end]);
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// Nombre de champs à remplir d'un gabarit (contrôle des catalogues).
pub fn placeholders(template: &str) -> usize {
    let mut n = 0;
    let mut rest = template;
    while let Some(i) = rest.find('{') {
        let after = &rest[i + 1..];
        if let Some(tail) = after.strip_prefix('{') {
            rest = tail;
            continue;
        }
        match after.find('}') {
            Some(end) if parse_placeholder(&after[..end]).is_some() => {
                n += 1;
                rest = &after[end + 1..];
            }
            _ => rest = after,
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Lang, with_lang};

    #[test]
    fn rust_like_placeholders() {
        with_lang(Lang::EN, || {
            assert_eq!(format("{} of {}", &[&1, &"two"]), "1 of two");
            assert_eq!(format("{1} then {0}", &[&"a", &"b"]), "b then a");
            assert_eq!(format("{:.2} dB", &[&-3.256f32]), "-3.26 dB");
            assert_eq!(format("{:+.1}", &[&2.0f64]), "+2.0");
            assert_eq!(format("{:+}", &[&3]), "+3");
            assert_eq!(format("[{:>4}]", &[&7]), "[   7]");
            assert_eq!(format("[{:<4}]", &[&"ab"]), "[ab  ]");
            assert_eq!(format("[{:^5}]", &[&"ab"]), "[ ab  ]");
            assert_eq!(format("{:03}", &[&7]), "007");
            assert_eq!(format("{:*>3}", &[&'x']), "**x");
            assert_eq!(format("{{x}} {}", &[&String::from("y")]), "{x} y");
            assert_eq!(format("{} {}", &[&1]), "1 {?}");
            assert_eq!(format("{nope}", &[]), "{nope}");
        });
    }

    #[test]
    fn decimal_separator_follows_the_language() {
        let fr = Lang::from_code("fr").expect("fr");
        with_lang(fr, || {
            assert_eq!(format("{:.1} BPM", &[&120.5f32]), "120,5 BPM");
            assert_eq!(format("{}", &[&12u32]), "12");
        });
    }

    #[test]
    fn counts_placeholders() {
        assert_eq!(placeholders("a {} b {:.1} {{c}} {0}"), 3);
        assert_eq!(placeholders("none"), 0);
    }
}
