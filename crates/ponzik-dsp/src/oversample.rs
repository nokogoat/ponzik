use std::f32::consts::{PI, TAU};

const TAPS: usize = 31;
const CENTER: usize = TAPS / 2;

/// Retard introduit par un aller-retour 2× (en échantillons à la fréquence de base).
pub const LATENCY: usize = CENTER;

/// Demi-bande RIF à sinus cardinal fenêtré (Blackman) : un coefficient sur deux
/// est nul, la coupure est à un quart de la fréquence suréchantillonnée.
#[derive(Clone, Debug)]
struct Halfband {
    kernel: [f32; TAPS],
    hist: [f32; 2 * TAPS],
    pos: usize,
}

impl Halfband {
    fn new() -> Self {
        let mut kernel = [0.0; TAPS];
        for (i, h) in kernel.iter_mut().enumerate() {
            let k = i as f32 - CENTER as f32;
            let sinc = if i == CENTER { 0.5 } else { (PI * k / 2.0).sin() / (PI * k) };
            let w = i as f32 / (TAPS - 1) as f32;
            let blackman = 0.42 - 0.5 * (TAU * w).cos() + 0.08 * (2.0 * TAU * w).cos();
            *h = sinc * blackman;
        }
        let sum: f32 = kernel.iter().sum();
        kernel.iter_mut().for_each(|h| *h /= sum);
        Self { kernel, hist: [0.0; 2 * TAPS], pos: 0 }
    }

    #[inline]
    fn push(&mut self, x: f32) {
        self.pos = if self.pos == 0 { TAPS - 1 } else { self.pos - 1 };
        self.hist[self.pos] = x;
        self.hist[self.pos + TAPS] = x;
    }

    #[inline]
    fn output(&self) -> f32 {
        let window = &self.hist[self.pos..self.pos + TAPS];
        self.kernel.iter().zip(window).map(|(h, x)| h * x).sum()
    }

    fn reset(&mut self) {
        self.hist = [0.0; 2 * TAPS];
    }
}

/// Suréchantillonnage 2× autour d'une non-linéarité, pour réduire le repliement.
#[derive(Clone, Debug)]
pub struct Oversampler2x {
    up: Halfband,
    down: Halfband,
}

impl Default for Oversampler2x {
    fn default() -> Self {
        Self { up: Halfband::new(), down: Halfband::new() }
    }
}

impl Oversampler2x {
    pub fn reset(&mut self) {
        self.up.reset();
        self.down.reset();
    }

    /// Applique `f` aux deux échantillons suréchantillonnés ; sortie retardée de [`LATENCY`].
    #[inline]
    pub fn process(&mut self, x: f32, mut f: impl FnMut(f32) -> f32) -> f32 {
        self.up.push(x);
        let a = f(2.0 * self.up.output());
        self.up.push(0.0);
        let b = f(2.0 * self.up.output());
        self.down.push(a);
        let y = self.down.output();
        self.down.push(b);
        y
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_roundtrip_is_a_pure_delay() {
        let mut os = Oversampler2x::default();
        let x = |n: usize| (TAU * 1_000.0 * n as f32 / 48_000.0).sin();
        for n in 0..2_000 {
            let y = os.process(x(n), |v| v);
            if n > 100 {
                assert!((y - x(n - LATENCY)).abs() < 0.01, "{n} : {y}");
            }
        }
    }

    #[test]
    fn images_are_rejected() {
        // Un sinus à 0,4 × la fréquence suréchantillonnée est dans la bande coupée.
        let mut hb = Halfband::new();
        let mut peak = 0.0f32;
        for n in 0..4_000 {
            hb.push((TAU * 0.4 * n as f32).sin());
            if n > 100 {
                peak = peak.max(hb.output().abs());
            }
        }
        assert!(peak < 0.01, "{peak}");
    }
}
