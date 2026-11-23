//! Thèmes de couleur. « Ponzik » est le thème maison : nuit sarcelle profonde,
//! vert vif pour ce qui joue, sarcelle glacée pour la sélection.

use ponzik_ui::Color;

#[derive(Clone, Copy, Debug)]
pub struct Theme {
    /// Identifiant enregistré dans les réglages (ne change jamais).
    pub name: &'static str,
    /// Nom affiché, en anglais, traduit par `tr`.
    pub label: &'static str,
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
    pub meter: Color,
    pub pad_a: Color,
    pub pad_b: Color,
    pub pad_head: Color,
}

const fn hairline(text: Color) -> Color {
    Color::rgba(text.r, text.g, text.b, 20)
}

pub const PONZIK: Theme = {
    let text = Color::hex(0xe6efec);
    Theme {
        name: "Ponzik",
        label: ponzik_i18n::tr_noop("Ponzik"),
        bg_deep: Color::hex(0x090c0c),
        bg: Color::hex(0x0e1212),
        panel: Color::hex(0x131817),
        strip: Color::hex(0x171d1c),
        raised: Color::hex(0x1c2322),
        raised_hi: Color::hex(0x26302f),
        well: Color::hex(0x0a0d0d),
        hairline: hairline(text),
        text,
        text2: Color::hex(0x98a7a3),
        muted: Color::hex(0x82928e),
        accent: Color::hex(0x3fd45a),
        accent_deep: Color::hex(0x23a83a),
        accent_hot: Color::hex(0xc2ffb9),
        on_accent: Color::hex(0x04230b),
        select: Color::hex(0x3ccfc4),
        negative: Color::hex(0xe5484d),
        meter: Color::hex(0xcfd8d5),
        pad_a: Color::hex(0x192020),
        pad_b: Color::hex(0x1f2827),
        pad_head: Color::hex(0x2e3b39),
    }
};

pub const GLACE: Theme = {
    let text = Color::hex(0xe8eef2);
    Theme {
        name: "Glace",
        label: ponzik_i18n::tr_noop("Ice"),
        bg_deep: Color::hex(0x0a0c10),
        bg: Color::hex(0x0e1116),
        panel: Color::hex(0x13171d),
        strip: Color::hex(0x171c23),
        raised: Color::hex(0x1c222a),
        raised_hi: Color::hex(0x262e38),
        well: Color::hex(0x0a0c10),
        hairline: hairline(text),
        text,
        text2: Color::hex(0x9ba5b3),
        muted: Color::hex(0x848e9e),
        accent: Color::hex(0x3ccfe0),
        accent_deep: Color::hex(0x1f9fb3),
        accent_hot: Color::hex(0xc4f6ff),
        on_accent: Color::hex(0x032029),
        select: Color::hex(0x9b8cff),
        negative: Color::hex(0xe5484d),
        meter: Color::hex(0xd2d8e0),
        pad_a: Color::hex(0x181d24),
        pad_b: Color::hex(0x1e242c),
        pad_head: Color::hex(0x2e3744),
    }
};

pub const AMBRE: Theme = {
    let text = Color::hex(0xededed);
    Theme {
        name: "Ambre",
        label: ponzik_i18n::tr_noop("Amber"),
        bg_deep: Color::hex(0x0f0f10),
        bg: Color::hex(0x141415),
        panel: Color::hex(0x19191b),
        strip: Color::hex(0x1d1d1f),
        raised: Color::hex(0x232325),
        raised_hi: Color::hex(0x2c2c2f),
        well: Color::hex(0x111112),
        hairline: hairline(text),
        text,
        text2: Color::hex(0xa1a1a6),
        muted: Color::hex(0x8c8c92),
        accent: Color::hex(0xf5a524),
        accent_deep: Color::hex(0xe38d14),
        accent_hot: Color::hex(0xffd68a),
        on_accent: Color::hex(0x1f1406),
        select: Color::hex(0x7cb7ff),
        negative: Color::hex(0xe8525a),
        meter: Color::hex(0xcfcfd2),
        pad_a: Color::hex(0x202022),
        pad_b: Color::hex(0x27272a),
        pad_head: Color::hex(0x3a3a3e),
    }
};

pub const THEMES: [Theme; 3] = [PONZIK, GLACE, AMBRE];

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
        for th in THEMES {
            for (bg, name) in [(th.bg, "bg"), (th.panel, "panel"), (th.raised, "raised"), (th.raised_hi, "raised_hi")] {
                // Text and secondary text: WCAG AA for small text everywhere.
                assert!(contrast(th.text, bg) >= 7.0, "{} text on {name}", th.name);
                assert!(contrast(th.text2, bg) >= 4.5, "{} text2 on {name}", th.name);
                // Hints and units: AA on the surfaces, nearly so on hovered ones.
                let min = if name == "raised_hi" { 4.0 } else { 4.5 };
                assert!(contrast(th.muted, bg) >= min, "{} muted on {name}: {:.2}", th.name, contrast(th.muted, bg));
            }
            assert!(contrast(th.on_accent, th.accent) >= 4.5, "{} text on the accent", th.name);
            // Controls and marks (non-text): 3:1 against the panels.
            for col in [th.accent, th.select, th.negative] {
                assert!(contrast(col, th.panel) >= 3.0, "{}", th.name);
            }
            assert!(contrast(th.negative, th.panel) >= 4.5, "{} errors", th.name);
        }
    }
}
