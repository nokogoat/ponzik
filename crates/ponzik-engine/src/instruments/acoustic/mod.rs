//! Modeled acoustic instruments: guitars, bowed strings, brass and winds,
//! all physical models (digital waveguides) built on [`wg`].

mod guitar;
pub(crate) mod wg;

pub use guitar::Guitar;
