//! Thèmes. « Ponzik » est le thème maison : nuit sarcelle profonde, vert vif
//! pour ce qui joue, sarcelle glacée pour la sélection.
//!
//! Tout ce qui se voit est un jeton nommé du [`Theme`] (couleurs, rayons,
//! contours, ombres, taille du texte, densité, vitesse des animations), listé
//! dans [`TOKENS`] sous son nom CSS (`--accent`, `--radius-md`…). Les jetons
//! « dérivés » se calculent à partir des autres tant qu'un thème ne les fixe pas :
//! changer `--text` change aussi `--hairline`, `--tooltip-text`…
//! Les fichiers de thème (sous-ensemble de CSS) sont lus dans `themecss.rs`.

use std::cell::Cell;

use ponzik_i18n::tr_noop;
use ponzik_ui::Color;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Theme {
    pub bg_deep: Color,
    pub bg: Color,
    pub panel: Color,
    pub strip: Color,
    pub raised: Color,
    pub raised_hi: Color,
    pub well: Color,
    pub hairline: Color,
    pub text: Color,
    pub text2: Color,
    pub muted: Color,
    /// Ce qui joue : pads allumés, notes, lecture.
    pub accent: Color,
    pub accent_deep: Color,
    pub accent_hot: Color,
    pub on_accent: Color,
    /// Sélection et repères (tête de lecture, rectangles de sélection).
    pub select: Color,
    pub negative: Color,
    pub warning: Color,
    pub frost: Color,
    pub highlight: Color,
    pub shadow: Color,
    pub meter: Color,
    pub meter_hot: Color,
    pub meter_clip: Color,
    pub meter_bg: Color,
    pub pad_a: Color,
    pub pad_b: Color,
    pub pad_head: Color,
    pub playhead: Color,
    /// Couleurs des patterns (et des pistes), des prises et des repères.
    pub patterns: [Color; 8],
    pub takes: [Color; 8],
    pub markers: [Color; 8],
    pub knob_track: Color,
    pub knob_arc: Color,
    pub knob_cap: Color,
    pub knob_cap_hover: Color,
    pub knob_pointer: Color,
    pub button_hover: Color,
    pub tooltip_bg: Color,
    pub tooltip_text: Color,
    pub tooltip_text2: Color,
    pub toast_bg: Color,
    pub toast_text: Color,
    pub toast_dot: Color,
    pub piano_row: Color,
    pub piano_row_black: Color,
    pub piano_grid: Color,
    pub note_low: Color,
    pub note: Color,
    pub note_hot: Color,
    pub key_white: Color,
    pub key_black: Color,
    pub lane_bg: Color,
    pub lane_alt: Color,
    pub playlist_grid: Color,
    pub wave: Color,
    /// Rayons des coins (petit, moyen, grand) qui remplacent 4, 8 et 16 px.
    pub radius: [f32; 3],
    pub border_width: f32,
    pub shadow_strength: f32,
    pub font_scale: f32,
    /// Espacement des listes et des rangées : 0,85 compact, 1 normal, 1,15 aéré.
    pub density: f32,
    /// 1 normale, 0 aucune animation.
    pub anim_speed: f32,
}

pub const DEFAULT_PATTERNS: [Color; 8] = hexes([0x3fd45a, 0x3ccfc4, 0x9be15d, 0x2fb6e0, 0x67e8a4, 0xc7e85a, 0x5ad1e8, 0x8ce07a]);
const DEFAULT_TAKES: [Color; 8] = hexes([0x3ccfc4, 0xf0b44a, 0xa98cf0, 0x9be15d, 0xe878b4, 0x2fb6e0, 0xf08a5d, 0x6b7cf0]);
const DEFAULT_MARKERS: [Color; 8] = hexes([0x3fd45a, 0x3ccfc4, 0xf0b44a, 0xe878b4, 0x2fb6e0, 0xa98cf0, 0xf08a5d, 0xc7e85a]);

const fn hexes(v: [u32; 8]) -> [Color; 8] {
    let mut out = [Color::TRANSPARENT; 8];
    let mut i = 0;
    while i < 8 {
        out[i] = Color::hex(v[i]);
        i += 1;
    }
    out
}

pub const DENSITY_COMPACT: f32 = 0.85;
pub const DENSITY_COMFORTABLE: f32 = 1.15;

/// Valeurs communes à tous les thèmes intégrés ; les jetons dérivés y sont
/// provisoires (recalculés par [`Theme::resolve`]).
const BLANK: Theme = Theme {
    bg_deep: Color::TRANSPARENT,
    bg: Color::TRANSPARENT,
    panel: Color::TRANSPARENT,
    strip: Color::TRANSPARENT,
    raised: Color::TRANSPARENT,
    raised_hi: Color::TRANSPARENT,
    well: Color::TRANSPARENT,
    hairline: Color::TRANSPARENT,
    text: Color::TRANSPARENT,
    text2: Color::TRANSPARENT,
    muted: Color::TRANSPARENT,
    accent: Color::TRANSPARENT,
    accent_deep: Color::TRANSPARENT,
    accent_hot: Color::TRANSPARENT,
    on_accent: Color::TRANSPARENT,
    select: Color::TRANSPARENT,
    negative: Color::hex(0xe5484d),
    warning: Color::hex(0xf0b44a),
    frost: Color::hex(0x8ee7f2),
    highlight: Color::hex(0xffffff),
    shadow: Color::hex(0x000000),
    meter: Color::TRANSPARENT,
    meter_hot: Color::TRANSPARENT,
    meter_clip: Color::TRANSPARENT,
    meter_bg: Color::TRANSPARENT,
    pad_a: Color::TRANSPARENT,
    pad_b: Color::TRANSPARENT,
    pad_head: Color::TRANSPARENT,
    playhead: Color::TRANSPARENT,
    patterns: DEFAULT_PATTERNS,
    takes: DEFAULT_TAKES,
    markers: DEFAULT_MARKERS,
    knob_track: Color::TRANSPARENT,
    knob_arc: Color::TRANSPARENT,
    knob_cap: Color::TRANSPARENT,
    knob_cap_hover: Color::hex(0x34403e),
    knob_pointer: Color::TRANSPARENT,
    button_hover: Color::hex(0x3a4644),
    tooltip_bg: Color::TRANSPARENT,
    tooltip_text: Color::TRANSPARENT,
    tooltip_text2: Color::TRANSPARENT,
    toast_bg: Color::TRANSPARENT,
    toast_text: Color::TRANSPARENT,
    toast_dot: Color::TRANSPARENT,
    piano_row: Color::TRANSPARENT,
    piano_row_black: Color::TRANSPARENT,
    piano_grid: Color::TRANSPARENT,
    note_low: Color::TRANSPARENT,
    note: Color::TRANSPARENT,
    note_hot: Color::TRANSPARENT,
    key_white: Color::TRANSPARENT,
    key_black: Color::TRANSPARENT,
    lane_bg: Color::TRANSPARENT,
    lane_alt: Color::TRANSPARENT,
    playlist_grid: Color::TRANSPARENT,
    wave: Color::TRANSPARENT,
    radius: ponzik_ui::DEFAULT_RADII,
    border_width: 1.0,
    shadow_strength: 1.0,
    font_scale: 1.0,
    density: 1.0,
    anim_speed: 1.0,
};

/// Thème intégré : identifiant enregistré dans les réglages (ne change jamais),
/// nom affiché (anglais, traduit par `tr`) et valeurs de base.
pub struct Builtin {
    pub id: &'static str,
    pub label: &'static str,
    pub raw: Theme,
}

pub const PONZIK_RAW: Theme = Theme {
    bg_deep: Color::hex(0x090c0c),
    bg: Color::hex(0x0e1212),
    panel: Color::hex(0x131817),
    strip: Color::hex(0x171d1c),
    raised: Color::hex(0x1c2322),
    raised_hi: Color::hex(0x26302f),
    well: Color::hex(0x0a0d0d),
    text: Color::hex(0xe6efec),
    text2: Color::hex(0x98a7a3),
    muted: Color::hex(0x82928e),
    accent: Color::hex(0x3fd45a),
    accent_deep: Color::hex(0x23a83a),
    accent_hot: Color::hex(0xc2ffb9),
    on_accent: Color::hex(0x04230b),
    select: Color::hex(0x3ccfc4),
    meter: Color::hex(0xcfd8d5),
    pad_a: Color::hex(0x192020),
    pad_b: Color::hex(0x1f2827),
    pad_head: Color::hex(0x2e3b39),
    ..BLANK
};

const GLACE_RAW: Theme = Theme {
    bg_deep: Color::hex(0x0a0c10),
    bg: Color::hex(0x0e1116),
    panel: Color::hex(0x13171d),
    strip: Color::hex(0x171c23),
    raised: Color::hex(0x1c222a),
    raised_hi: Color::hex(0x262e38),
    well: Color::hex(0x0a0c10),
    text: Color::hex(0xe8eef2),
    text2: Color::hex(0x9ba5b3),
    muted: Color::hex(0x848e9e),
    accent: Color::hex(0x3ccfe0),
    accent_deep: Color::hex(0x1f9fb3),
    accent_hot: Color::hex(0xc4f6ff),
    on_accent: Color::hex(0x032029),
    select: Color::hex(0x9b8cff),
    meter: Color::hex(0xd2d8e0),
    pad_a: Color::hex(0x181d24),
    pad_b: Color::hex(0x1e242c),
    pad_head: Color::hex(0x2e3744),
    ..BLANK
};

const AMBRE_RAW: Theme = Theme {
    bg_deep: Color::hex(0x0f0f10),
    bg: Color::hex(0x141415),
    panel: Color::hex(0x19191b),
    strip: Color::hex(0x1d1d1f),
    raised: Color::hex(0x232325),
    raised_hi: Color::hex(0x2c2c2f),
    well: Color::hex(0x111112),
    text: Color::hex(0xededed),
    text2: Color::hex(0xa1a1a6),
    muted: Color::hex(0x8c8c92),
    accent: Color::hex(0xf5a524),
    accent_deep: Color::hex(0xe38d14),
    accent_hot: Color::hex(0xffd68a),
    on_accent: Color::hex(0x1f1406),
    select: Color::hex(0x7cb7ff),
    meter: Color::hex(0xcfcfd2),
    pad_a: Color::hex(0x202022),
    pad_b: Color::hex(0x27272a),
    pad_head: Color::hex(0x3a3a3e),
    negative: Color::hex(0xe8525a),
    ..BLANK
};

pub const BUILTINS: [Builtin; 3] = [
    Builtin { id: "Ponzik", label: tr_noop("Ponzik"), raw: PONZIK_RAW },
    Builtin { id: "Glace", label: tr_noop("Ice"), raw: GLACE_RAW },
    Builtin { id: "Ambre", label: tr_noop("Amber"), raw: AMBRE_RAW },
];

/// Le thème maison, jetons dérivés compris.
pub fn ponzik() -> Theme {
    Theme::resolve(&PONZIK_RAW, &[])
}

pub const WHITE: Color = Color::hex(0xffffff);

pub const TOP_BAR: f32 = 56.0;
pub const SIDEBAR: f32 = 248.0;
pub const PANEL: f32 = 272.0;
pub const PAD: f32 = 16.0;

/// Constantes de temps des animations (s).
pub const T_PRESS: f32 = 0.04;
pub const T_HOVER: f32 = 0.05;
pub const T_HEAD: f32 = 0.07;
pub const T_FLASH: f32 = 0.11;
/// Changements de vue et ouverture des panneaux.
pub const T_LAYOUT: f32 = 0.075;

// --- Palette des patterns ---

thread_local! {
    /// Couleurs des patterns du thème actif, pour les fonctions libres qui
    /// n'ont pas le thème sous la main (`pattern_color`).
    static PATTERNS: Cell<[Color; 8]> = const { Cell::new(DEFAULT_PATTERNS) };
}

pub fn set_pattern_palette(p: [Color; 8]) {
    PATTERNS.with(|c| c.set(p));
}

pub fn pattern_palette(i: usize) -> Color {
    PATTERNS.with(|c| c.get()[i % 8])
}

// --- Jetons ---

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Val {
    Color(Color),
    Num(f32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Color,
    /// Longueur en pixels (`10px`).
    Px,
    /// Facteur sans unité.
    Factor,
    /// `compact`, `normal`, `comfortable` (ou un facteur).
    Density,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    Surfaces,
    Text,
    Accent,
    Signals,
    Palettes,
    Controls,
    Overlays,
    Mixer,
    Piano,
    Playlist,
    Shape,
}

impl Group {
    pub const ALL: [Group; 11] = [
        Group::Surfaces,
        Group::Text,
        Group::Accent,
        Group::Signals,
        Group::Palettes,
        Group::Controls,
        Group::Overlays,
        Group::Mixer,
        Group::Piano,
        Group::Playlist,
        Group::Shape,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Group::Surfaces => tr_noop("Surfaces"),
            Group::Text => tr_noop("Text"),
            Group::Accent => tr_noop("Accent"),
            Group::Signals => tr_noop("Signals"),
            Group::Palettes => tr_noop("Palettes"),
            Group::Controls => tr_noop("Controls"),
            Group::Overlays => tr_noop("Tooltips and toasts"),
            Group::Mixer => tr_noop("Mixer"),
            Group::Piano => tr_noop("Piano roll"),
            Group::Playlist => tr_noop("Playlist"),
            Group::Shape => tr_noop("Shape and motion"),
        }
    }
}

pub struct TokenDef {
    /// Nom CSS, sans les deux tirets.
    pub name: &'static str,
    pub group: Group,
    pub kind: Kind,
    /// Bornes des jetons numériques.
    pub range: (f32, f32),
    pub get: fn(&Theme) -> Val,
    pub set: fn(&mut Theme, Val),
    /// Valeur calculée à partir des autres jetons quand le thème ne la fixe pas.
    pub derive: Option<fn(&Theme) -> Val>,
}

macro_rules! color {
    ($name:literal, $group:ident, $f:ident $([$i:literal])?) => {
        color!(@ $name, $group, None, $f $([$i])?)
    };
    ($name:literal, $group:ident, $f:ident, $d:expr) => {
        color!(@ $name, $group, Some($d), $f)
    };
    (@ $name:literal, $group:ident, $derive:expr, $f:ident $([$i:literal])?) => {
        TokenDef {
            name: $name,
            group: Group::$group,
            kind: Kind::Color,
            range: (0.0, 0.0),
            get: |t| Val::Color(t.$f$([$i])?),
            set: |t, v| {
                if let Val::Color(c) = v {
                    t.$f$([$i])? = c;
                }
            },
            derive: $derive,
        }
    };
}

macro_rules! num {
    ($name:literal, $kind:ident, $lo:literal, $hi:literal, $f:ident $([$i:literal])?) => {
        TokenDef {
            name: $name,
            group: Group::Shape,
            kind: Kind::$kind,
            range: ($lo, $hi),
            get: |t| Val::Num(t.$f$([$i])?),
            set: |t, v| {
                if let Val::Num(x) = v {
                    t.$f$([$i])? = x.clamp($lo, $hi);
                }
            },
            derive: None,
        }
    };
}

fn c(c: Color) -> Val {
    Val::Color(c)
}

/// Tous les jetons, dans l'ordre de calcul : un jeton dérivé vient après ceux
/// dont il dépend.
pub static TOKENS: &[TokenDef] = &[
    color!("bg-deep", Surfaces, bg_deep),
    color!("bg", Surfaces, bg),
    color!("panel", Surfaces, panel),
    color!("strip", Surfaces, strip),
    color!("raised", Surfaces, raised),
    color!("raised-hi", Surfaces, raised_hi),
    color!("well", Surfaces, well),
    color!("text", Text, text),
    color!("text-2", Text, text2),
    color!("muted", Text, muted),
    color!("hairline", Surfaces, hairline, |t| c(Color::rgba(t.text.r, t.text.g, t.text.b, 20))),
    color!("highlight", Surfaces, highlight),
    color!("shadow", Surfaces, shadow),
    color!("accent", Accent, accent),
    color!("accent-deep", Accent, accent_deep),
    color!("accent-hot", Accent, accent_hot),
    color!("on-accent", Accent, on_accent),
    color!("select", Accent, select),
    color!("playhead", Accent, playhead, |t| c(t.select)),
    color!("negative", Signals, negative),
    color!("warning", Signals, warning),
    color!("frost", Signals, frost),
    color!("meter", Mixer, meter),
    color!("meter-hot", Mixer, meter_hot, |t| c(t.accent)),
    color!("meter-clip", Mixer, meter_clip, |t| c(t.negative)),
    color!("meter-bg", Mixer, meter_bg, |t| c(t.well)),
    color!("pad-a", Controls, pad_a),
    color!("pad-b", Controls, pad_b),
    color!("pad-head", Controls, pad_head),
    color!("knob-track", Controls, knob_track, |t| c(t.raised_hi)),
    color!("knob-arc", Controls, knob_arc, |t| c(t.accent)),
    color!("knob-cap", Controls, knob_cap, |t| c(t.raised_hi)),
    color!("knob-cap-hover", Controls, knob_cap_hover),
    color!("knob-pointer", Controls, knob_pointer, |t| c(t.text)),
    color!("button-hover", Controls, button_hover),
    color!("pattern-1", Palettes, patterns[0]),
    color!("pattern-2", Palettes, patterns[1]),
    color!("pattern-3", Palettes, patterns[2]),
    color!("pattern-4", Palettes, patterns[3]),
    color!("pattern-5", Palettes, patterns[4]),
    color!("pattern-6", Palettes, patterns[5]),
    color!("pattern-7", Palettes, patterns[6]),
    color!("pattern-8", Palettes, patterns[7]),
    color!("take-1", Palettes, takes[0]),
    color!("take-2", Palettes, takes[1]),
    color!("take-3", Palettes, takes[2]),
    color!("take-4", Palettes, takes[3]),
    color!("take-5", Palettes, takes[4]),
    color!("take-6", Palettes, takes[5]),
    color!("take-7", Palettes, takes[6]),
    color!("take-8", Palettes, takes[7]),
    color!("marker-1", Palettes, markers[0]),
    color!("marker-2", Palettes, markers[1]),
    color!("marker-3", Palettes, markers[2]),
    color!("marker-4", Palettes, markers[3]),
    color!("marker-5", Palettes, markers[4]),
    color!("marker-6", Palettes, markers[5]),
    color!("marker-7", Palettes, markers[6]),
    color!("marker-8", Palettes, markers[7]),
    color!("tooltip-bg", Overlays, tooltip_bg, |t| c(t.raised_hi.mix(t.bg_deep, 0.15))),
    color!("tooltip-text", Overlays, tooltip_text, |t| c(t.text)),
    color!("tooltip-text-2", Overlays, tooltip_text2, |t| c(t.text2)),
    color!("toast-bg", Overlays, toast_bg, |t| c(t.raised_hi)),
    color!("toast-text", Overlays, toast_text, |t| c(t.text)),
    color!("toast-dot", Overlays, toast_dot, |t| c(t.accent)),
    color!("piano-row", Piano, piano_row, |t| c(t.panel.mix(t.raised, 0.35))),
    color!("piano-row-black", Piano, piano_row_black, |t| c(t.well)),
    color!("piano-grid", Piano, piano_grid, |t| c(t.hairline)),
    color!("note-low", Piano, note_low, |t| c(t.accent_deep)),
    color!("note", Piano, note, |t| c(t.accent)),
    color!("note-hot", Piano, note_hot, |t| c(t.accent_hot)),
    color!("key-white", Piano, key_white, |t| c(t.text.mix(t.raised_hi, 0.12))),
    color!("key-black", Piano, key_black, |t| c(t.bg_deep)),
    color!("lane-bg", Playlist, lane_bg, |t| c(t.well)),
    color!("lane-alt", Playlist, lane_alt, |t| c(t.panel.alpha(0.35))),
    color!("playlist-grid", Playlist, playlist_grid, |t| c(t.hairline)),
    color!("wave", Playlist, wave, |t| c(t.text)),
    num!("radius-sm", Px, 0.0, 32.0, radius[0]),
    num!("radius-md", Px, 0.0, 32.0, radius[1]),
    num!("radius-lg", Px, 0.0, 48.0, radius[2]),
    num!("border-width", Px, 0.0, 4.0, border_width),
    num!("shadow-strength", Factor, 0.0, 3.0, shadow_strength),
    num!("font-scale", Factor, 0.75, 1.5, font_scale),
    num!("density", Density, 0.7, 1.4, density),
    num!("animation-speed", Factor, 0.0, 3.0, anim_speed),
];

/// Indice d'un jeton par son nom CSS (sans `--`).
pub fn token_index(name: &str) -> Option<usize> {
    TOKENS.iter().position(|t| t.name == name)
}

impl Theme {
    /// Thème complet : `raw` (un thème intégré), puis `overrides` (indice de
    /// jeton, valeur), les jetons dérivés non fixés étant recalculés dans l'ordre.
    pub fn resolve(raw: &Theme, overrides: &[(usize, Val)]) -> Theme {
        let mut t = *raw;
        let mut fixed = [false; 128];
        for &(i, v) in overrides {
            if let Some(def) = TOKENS.get(i) {
                (def.set)(&mut t, v);
                fixed[i] = true;
            }
        }
        for (i, def) in TOKENS.iter().enumerate() {
            if let Some(d) = def.derive
                && !fixed[i]
            {
                let v = d(&t);
                (def.set)(&mut t, v);
            }
        }
        t
    }

    pub fn get(&self, i: usize) -> Val {
        (TOKENS[i].get)(self)
    }

    /// Ombre portée d'opacité `a` (0 à 255), selon la force des ombres du thème.
    pub fn shade(&self, a: f32) -> Color {
        let k = self.shadow_strength * self.shadow.a as f32 / 255.0;
        Color::rgba(self.shadow.r, self.shadow.g, self.shadow.b, (a * k) as u8)
    }

    /// Fond clair : le thème est en mode jour.
    pub fn is_light(&self) -> bool {
        luminance(self.bg) > 0.4
    }

    /// Longueur multipliée par la densité, arrondie au pixel.
    pub fn dense(&self, v: f32) -> f32 {
        if self.density == 1.0 { v } else { (v * self.density).round() }
    }
}

// --- Couleurs : conversions et contraste ---

/// `#rrggbb`, ou `#rrggbbaa` si la couleur est transparente.
pub fn hex_string(c: Color) -> String {
    if c.a == 255 { format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b) } else { format!("#{:02x}{:02x}{:02x}{:02x}", c.r, c.g, c.b, c.a) }
}

fn channel(v: u8) -> f32 {
    let v = v as f32 / 255.0;
    if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

/// Luminance relative WCAG 2.
pub fn luminance(c: Color) -> f32 {
    0.2126 * channel(c.r) + 0.7152 * channel(c.g) + 0.0722 * channel(c.b)
}

/// Rapport de contraste WCAG (1 à 21) de `fg` posé sur `bg` (la transparence de
/// `fg` est mélangée au fond).
pub fn contrast(fg: Color, bg: Color) -> f32 {
    let fg = Color::rgba(bg.r, bg.g, bg.b, 255).mix(Color::rgba(fg.r, fg.g, fg.b, 255), fg.a as f32 / 255.0);
    let (a, b) = (luminance(fg), luminance(bg));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// Teinte (degrés), saturation et valeur (0 à 1).
pub fn to_hsv(c: Color) -> (f32, f32, f32) {
    let (r, g, b) = (c.r as f32 / 255.0, c.g as f32 / 255.0, c.b as f32 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let h = hue(r, g, b, max, d);
    let s = if max > 0.0 { d / max } else { 0.0 };
    (h, s, max)
}

fn hue(r: f32, g: f32, b: f32, max: f32, d: f32) -> f32 {
    if d <= 0.0 {
        return 0.0;
    }
    let h = if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    h * 60.0
}

pub fn from_hsv(h: f32, s: f32, v: f32, a: u8) -> Color {
    let h = h.rem_euclid(360.0) / 60.0;
    let (s, v) = (s.clamp(0.0, 1.0), v.clamp(0.0, 1.0));
    let c = v * s;
    let x = c * (1.0 - (h.rem_euclid(2.0) - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    let q = |v: f32| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    Color::rgba(q(r), q(g), q(b), a)
}

/// Teinte (degrés), saturation et luminosité (0 à 1).
pub fn to_hsl(c: Color) -> (f32, f32, f32) {
    let (r, g, b) = (c.r as f32 / 255.0, c.g as f32 / 255.0, c.b as f32 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let l = (max + min) * 0.5;
    let s = if d <= 0.0 { 0.0 } else { d / (1.0 - (2.0 * l - 1.0).abs()) };
    (hue(r, g, b, max, d), s.clamp(0.0, 1.0), l)
}

pub fn from_hsl(h: f32, s: f32, l: f32, a: u8) -> Color {
    let (s, l) = (s.clamp(0.0, 1.0), l.clamp(0.0, 1.0));
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let v = l + c * 0.5;
    let sv = if v > 0.0 { 2.0 * (1.0 - l / v) } else { 0.0 };
    from_hsv(h, sv, v, a)
}

/// Paire de jetons dont le contraste est trop faible pour être lu confortablement.
#[derive(Clone, Debug, PartialEq)]
pub struct ContrastIssue {
    pub fg: &'static str,
    pub bg: &'static str,
    pub ratio: f32,
    pub min: f32,
}

/// Vérifie les textes sur leurs fonds (WCAG : 4,5 pour le texte courant, 3 pour
/// le texte secondaire et les grands éléments).
pub fn contrast_issues(t: &Theme) -> Vec<ContrastIssue> {
    let pairs: [(&str, Color, &str, Color, f32); 10] = [
        ("text", t.text, "bg", t.bg, 4.5),
        ("text", t.text, "panel", t.panel, 4.5),
        ("text", t.text, "raised", t.raised, 4.5),
        ("text-2", t.text2, "panel", t.panel, 4.5),
        ("text-2", t.text2, "raised", t.raised, 3.0),
        ("muted", t.muted, "panel", t.panel, 3.0),
        ("on-accent", t.on_accent, "accent", t.accent, 4.5),
        ("accent", t.accent, "panel", t.panel, 3.0),
        ("tooltip-text", t.tooltip_text, "tooltip-bg", t.tooltip_bg, 4.5),
        ("toast-text", t.toast_text, "toast-bg", t.toast_bg, 4.5),
    ];
    pairs
        .iter()
        .filter_map(|&(fg, a, bg, b, min)| {
            let ratio = contrast(a, b);
            (ratio < min).then_some(ContrastIssue { fg, bg, ratio, min })
        })
        .collect()
}

/// Couleur de même teinte, éclaircie ou assombrie jusqu'à atteindre `min` sur `bg`.
pub fn ensure_contrast(fg: Color, bg: Color, min: f32) -> Color {
    if contrast(fg, bg) >= min {
        return fg;
    }
    let (h, s, l) = to_hsl(fg);
    let lighter = luminance(bg) < 0.18;
    let mut best = fg;
    for k in 1..=50 {
        let l2 = if lighter { l + (1.0 - l) * k as f32 / 50.0 } else { l * (1.0 - k as f32 / 50.0) };
        best = from_hsl(h, s, l2, fg.a);
        if contrast(best, bg) >= min {
            break;
        }
    }
    best
}

/// Jetons de base harmonieux tirés d'une couleur d'accent : surfaces de la même
/// teinte peu saturées, textes contrastés, sélection en couleur complémentaire.
/// `light` : thème clair.
pub fn generate_from_accent(accent: Color, light: bool) -> Vec<(&'static str, Color)> {
    let (h, s, l) = to_hsl(accent);
    let tint = (s * 0.22).min(0.18);
    let surf = |l: f32| from_hsl(h, tint, l, 255);
    let (ladder, text, text2, muted) = if light {
        ([0.86, 0.93, 0.975, 0.96, 0.985, 0.995, 0.90, 0.91, 0.88, 0.82], 0.10, 0.32, 0.42)
    } else {
        ([0.035, 0.06, 0.08, 0.095, 0.115, 0.165, 0.04, 0.11, 0.135, 0.205], 0.92, 0.63, 0.51)
    };
    let names = ["bg-deep", "bg", "panel", "strip", "raised", "raised-hi", "well", "pad-a", "pad-b", "pad-head"];
    let mut out: Vec<(&'static str, Color)> = names.iter().zip(ladder).map(|(&n, l)| (n, surf(l))).collect();
    let panel = surf(ladder[2]);
    let raised = surf(ladder[4]);
    let text_c = ensure_contrast(from_hsl(h, tint * 0.8, text, 255), raised, 7.0);
    out.push(("text", text_c));
    out.push(("text-2", ensure_contrast(from_hsl(h, tint * 0.9, text2, 255), panel, 4.6)));
    out.push(("muted", ensure_contrast(from_hsl(h, tint, muted, 255), panel, 3.1)));
    let accent = if light { ensure_contrast(accent, panel, 3.0) } else { accent };
    let (ah, asat, al) = to_hsl(accent);
    out.push(("accent", accent));
    out.push(("accent-deep", from_hsl(ah, asat, al * 0.78, 255)));
    out.push(("accent-hot", from_hsl(ah, asat.max(0.6), if light { al * 0.7 } else { 0.86 }, 255)));
    let dark_ink = from_hsl(ah, asat.min(0.8), 0.08, 255);
    // A dark ink of the accent's hue, else black or white (one of them always passes 4.5).
    let ink = [dark_ink, Color::hex(0x000000), WHITE].into_iter().find(|&c| contrast(c, accent) >= 4.5).unwrap_or(WHITE);
    out.push(("on-accent", ink));
    let sel = from_hsl(h + 150.0, s.clamp(0.55, 0.8), l.clamp(0.5, 0.62), 255);
    out.push(("select", if light { ensure_contrast(sel, panel, 3.0) } else { sel }));
    out.push(("meter", from_hsl(h, tint, if light { 0.35 } else { 0.83 }, 255)));
    out.push(("knob-cap-hover", surf(if light { 0.88 } else { 0.23 })));
    out.push(("button-hover", surf(if light { 0.85 } else { 0.25 })));
    out.push(("highlight", if light { Color::hex(0x000000) } else { WHITE }));
    out
}

/// Accent au hasard : teinte libre, saturation et luminosité lisibles.
pub fn random_accent(seed: u64) -> Color {
    let mut x = seed ^ 0x9e37_79b9_7f4a_7c15;
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
    x ^= x >> 33;
    let f = |shift: u32| ((x >> shift) & 0xffff) as f32 / 65535.0;
    from_hsl(f(0) * 360.0, 0.65 + 0.25 * f(16), 0.5 + 0.1 * f(32), 255)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WCAG relative luminance.
    fn luminance(c: Color) -> f64 {
        let lin = |v: u8| {
            let x = v as f64 / 255.0;
            if x <= 0.039_28 { x / 12.92 } else { ((x + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b)
    }

    fn contrast(a: Color, b: Color) -> f64 {
        let (la, lb) = (luminance(a), luminance(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    #[test]
    fn every_theme_reads_well() {
        for b in BUILTINS {
            let (th, id) = (Theme::resolve(&b.raw, &[]), b.id);
            for (bg, name) in [(th.bg, "bg"), (th.panel, "panel"), (th.raised, "raised"), (th.raised_hi, "raised_hi")] {
                // Text and secondary text: WCAG AA for small text everywhere.
                assert!(contrast(th.text, bg) >= 7.0, "{id} text on {name}");
                assert!(contrast(th.text2, bg) >= 4.5, "{id} text2 on {name}");
                // Hints and units: AA on the surfaces, nearly so on hovered ones.
                let min = if name == "raised_hi" { 4.0 } else { 4.5 };
                assert!(contrast(th.muted, bg) >= min, "{id} muted on {name}: {:.2}", contrast(th.muted, bg));
            }
            assert!(contrast(th.on_accent, th.accent) >= 4.5, "{id} text on the accent");
            // Controls and marks (non-text): 3:1 against the panels.
            for col in [th.accent, th.select, th.negative] {
                assert!(contrast(col, th.panel) >= 3.0, "{id}");
            }
            assert!(contrast(th.negative, th.panel) >= 4.5, "{id} errors");
        }
    }

    /// Les valeurs d'avant les jetons : les thèmes intégrés n'ont pas bougé d'un pixel.
    #[test]
    fn builtins_keep_their_colors() {
        let p = ponzik();
        assert_eq!(p.bg, Color::hex(0x0e1212));
        assert_eq!(p.accent, Color::hex(0x3fd45a));
        assert_eq!(p.hairline, Color::rgba(0xe6, 0xef, 0xec, 20));
        assert_eq!(p.negative, Color::hex(0xe5484d));
        assert_eq!(p.radius, [4.0, 8.0, 16.0]);
        assert_eq!((p.border_width, p.shadow_strength, p.font_scale, p.density, p.anim_speed), (1.0, 1.0, 1.0, 1.0, 1.0));
        // Les jetons dérivés valent les expressions qu'ils remplacent.
        assert_eq!(p.tooltip_bg, p.raised_hi.mix(p.bg_deep, 0.15));
        assert_eq!(p.piano_row, p.panel.mix(p.raised, 0.35));
        assert_eq!(p.key_white, p.text.mix(p.raised_hi, 0.12));
        assert_eq!(p.lane_alt, p.panel.alpha(0.35));
        assert_eq!(p.knob_cap_hover, Color::hex(0x34403e));
        assert_eq!(p.shade(90.0), Color::rgba(0, 0, 0, 90));
        assert_eq!(p.patterns[0], Color::hex(0x3fd45a));
        let g = Theme::resolve(&BUILTINS[1].raw, &[]);
        assert_eq!((g.accent, g.select, g.hairline), (Color::hex(0x3ccfe0), Color::hex(0x9b8cff), Color::rgba(0xe8, 0xee, 0xf2, 20)));
        let a = Theme::resolve(&BUILTINS[2].raw, &[]);
        assert_eq!((a.accent, a.on_accent, a.pad_head), (Color::hex(0xf5a524), Color::hex(0x1f1406), Color::hex(0x3a3a3e)));
    }

    #[test]
    fn token_names_are_unique_and_derivations_follow() {
        assert!(TOKENS.len() <= 128);
        for (i, a) in TOKENS.iter().enumerate() {
            assert!(TOKENS[i + 1..].iter().all(|b| b.name != a.name), "{}", a.name);
        }
        let text = token_index("text").unwrap();
        let t = Theme::resolve(&PONZIK_RAW, &[(text, Val::Color(Color::hex(0x102030)))]);
        assert_eq!(t.hairline, Color::rgba(0x10, 0x20, 0x30, 20));
        assert_eq!(t.tooltip_text, Color::hex(0x102030));
        // Un jeton dérivé fixé l'emporte, et ceux qui en dépendent le suivent.
        let hair = token_index("hairline").unwrap();
        let t = Theme::resolve(&PONZIK_RAW, &[(hair, Val::Color(Color::hex(0xff0000)))]);
        assert_eq!(t.piano_grid, Color::hex(0xff0000));
        // Les nombres restent dans leurs bornes.
        let fs = token_index("font-scale").unwrap();
        assert_eq!(Theme::resolve(&PONZIK_RAW, &[(fs, Val::Num(9.0))]).font_scale, 1.5);
    }

    #[test]
    fn contrast_ratios() {
        assert!((contrast(Color::hex(0x000000), Color::hex(0xffffff)) - 21.0).abs() < 0.01);
        assert!((contrast(Color::hex(0xffffff), Color::hex(0xffffff)) - 1.0).abs() < 1e-4);
        // #767676 sur blanc : le gris le plus clair qui passe 4,5.
        let r = contrast(Color::hex(0x767676), Color::hex(0xffffff));
        assert!((r - 4.54).abs() < 0.02, "{r}");
        assert!(contrast_issues(&ponzik()).iter().all(|i| i.fg != "text"));
        let mut bad = ponzik();
        bad.text = bad.panel;
        assert!(contrast_issues(&bad).iter().any(|i| i.fg == "text" && i.bg == "panel"));
    }

    #[test]
    fn color_space_round_trips() {
        for hex in [0x3fd45a, 0x0e1212, 0xf5a524, 0x7cb7ff, 0xffffff, 0x000000, 0x808080] {
            let col = Color::hex(hex);
            let (h, s, v) = to_hsv(col);
            assert_eq!(from_hsv(h, s, v, 255), col);
            let (h, s, l) = to_hsl(col);
            assert_eq!(from_hsl(h, s, l, 255), col);
        }
        assert_eq!(from_hsl(0.0, 1.0, 0.5, 255), Color::hex(0xff0000));
        assert_eq!(from_hsl(120.0, 1.0, 0.25, 255), Color::hex(0x008000));
    }

    #[test]
    fn generated_themes_are_readable() {
        for (seed, light) in [(1, false), (2, true), (3, false), (4, true), (5, false)] {
            let accent = random_accent(seed);
            let generated = generate_from_accent(accent, light);
            let overrides: Vec<(usize, Val)> = generated.iter().map(|(n, c)| (token_index(n).unwrap(), Val::Color(*c))).collect();
            let t = Theme::resolve(&PONZIK_RAW, &overrides);
            assert_eq!(t.is_light(), light);
            let issues = contrast_issues(&t);
            assert!(issues.is_empty(), "seed {seed}: {issues:?}");
        }
    }
}
