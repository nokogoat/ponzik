//! Modeled acoustic instruments: guitars, bowed strings, brass and winds,
//! all physical models (digital waveguides) built on [`wg`].

mod guitar;
mod section;
mod strings;
pub(crate) mod wg;

pub use guitar::Guitar;
pub use strings::Strings;
