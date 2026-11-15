//! Direction visuelle : matériel de studio la nuit. Neutres graphite, un seul
//! accent ambre (les LED des pads), la couleur réservée à ce qui joue.

use ponzik_ui::Color;

pub const BG_DEEP: Color = Color::hex(0x0f0f10);
pub const BG: Color = Color::hex(0x141415);
pub const CARD: Color = Color::hex(0x19191b);
pub const STRIP: Color = Color::hex(0x1d1d1f);
pub const RAISED: Color = Color::hex(0x232325);
pub const RAISED_HI: Color = Color::hex(0x2c2c2f);
pub const WELL: Color = Color::hex(0x111112);
/// Texte principal à 8 %.
pub const HAIRLINE: Color = Color::rgba(237, 237, 237, 20);

pub const TEXT: Color = Color::hex(0xededed);
pub const TEXT2: Color = Color::hex(0xa1a1a6);
pub const MUTED: Color = Color::hex(0x85858b);

pub const AMBER: Color = Color::hex(0xf5a524);
pub const AMBER_DEEP: Color = Color::hex(0xe38d14);
pub const AMBER_HOT: Color = Color::hex(0xffd68a);
pub const ON_AMBER: Color = Color::hex(0x1f1406);
pub const NEGATIVE: Color = Color::hex(0xe5484d);
pub const METER: Color = Color::hex(0xcfcfd2);

pub const PAD_A: Color = Color::hex(0x202022);
pub const PAD_B: Color = Color::hex(0x27272a);
pub const PAD_HEAD: Color = Color::hex(0x3a3a3e);
pub const WHITE: Color = Color::hex(0xffffff);

pub const TOP_BAR: f32 = 64.0;
pub const PAGE: f32 = 24.0;
pub const CARD_PAD: f32 = 20.0;
pub const CARD_RADIUS: f32 = 16.0;
pub const SECTION_GAP: f32 = 16.0;

pub const NAME_COL: f32 = 200.0;
pub const ROW_H: f32 = 56.0;
pub const ROW_GAP: f32 = 8.0;
pub const PAD_H: f32 = 48.0;
pub const PAD_GAP: f32 = 6.0;
pub const GROUP_GAP: f32 = 16.0;

pub const STRIP_W: f32 = 112.0;
pub const MASTER_W: f32 = 136.0;
pub const STRIP_GAP: f32 = 12.0;

/// Constantes de temps des animations (s) : retour visuel, survol, flash de pas.
pub const T_PRESS: f32 = 0.04;
pub const T_HOVER: f32 = 0.05;
pub const T_HEAD: f32 = 0.07;
pub const T_FLASH: f32 = 0.11;
