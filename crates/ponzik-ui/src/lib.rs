//! Rendu 2D maison de Ponzik, sans dépendance.
//!
//! - [`Canvas`] : tampon de pixels `0x00RRGGBB`, rectangles arrondis à bords
//!   analytiques, chemins et traits anti-aliasés.
//! - [`Fonts`] : lecture des polices TrueType (glyf, cmap, crénage GPOS),
//!   rastérisation et cache des glyphes.
//! - [`Anim`] : valeurs animées qui suivent leur cible en douceur.

// Les primitives de dessin prennent naturellement position, taille et couleur.
#![allow(clippy::too_many_arguments)]

mod anim;
mod canvas;
mod color;
mod font;
mod geom;
mod png;
mod raster;
mod text;

pub mod brand;
pub use anim::Anim;
pub use canvas::{Canvas, DEFAULT_RADII, Path};
pub use color::Color;
pub use geom::Rect;
pub use png::{encode_png, encode_png_rgba};
pub use text::{FontId, Fonts};

/// Polices embarquées (IBM Plex, licence OFL — voir `fonts/OFL.txt`).
pub mod embedded {
    pub const SANS_REGULAR: &[u8] = include_bytes!("../fonts/IBMPlexSans-Regular.ttf");
    pub const SANS_MEDIUM: &[u8] = include_bytes!("../fonts/IBMPlexSans-Medium.ttf");
    pub const MONO_REGULAR: &[u8] = include_bytes!("../fonts/IBMPlexMono-Regular.ttf");
    pub const MONO_MEDIUM: &[u8] = include_bytes!("../fonts/IBMPlexMono-Medium.ttf");
}
