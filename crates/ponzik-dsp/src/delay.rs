/// Ligne de retard circulaire, allouée une seule fois puis lue en position
/// fractionnaire (interpolation d'Hermite).
///
/// On lit avant d'écrire : `read(d)` rend l'échantillon écrit il y a `d` pas.
#[derive(Clone, Debug)]
pub struct DelayLine {
    buf: Vec<f32>,
    mask: usize,
    pos: usize,
    max: usize,
}

impl DelayLine {
    /// Alloue : à appeler hors du thread audio.
    pub fn new(max_delay: usize) -> Self {
        let len = (max_delay + 4).next_power_of_two();
        Self { buf: vec![0.0; len], mask: len - 1, pos: 0, max: max_delay.max(2) }
    }

    pub fn max_delay(&self) -> usize {
        self.max
    }

    pub fn clear(&mut self) {
        self.buf.fill(0.0);
    }

    #[inline]
    pub fn write(&mut self, x: f32) {
        self.buf[self.pos] = x;
        self.pos = (self.pos + 1) & self.mask;
    }

    #[inline]
    fn at(&self, delay: usize) -> f32 {
        self.buf[(self.pos + self.mask + 1 - delay) & self.mask]
    }

    /// Retard entier, borné à `[1, max_delay]`.
    #[inline]
    pub fn read(&self, delay: usize) -> f32 {
        self.at(delay.clamp(1, self.max))
    }

    /// Retard fractionnaire, borné à `[2, max_delay]`.
    #[inline]
    pub fn read_frac(&self, delay: f32) -> f32 {
        let d = delay.clamp(2.0, self.max as f32);
        let i = d as usize;
        let t = d - i as f32;
        let (xm1, x0, x1, x2) = (self.at(i - 1), self.at(i), self.at(i + 1), self.at(i + 2));
        let c1 = 0.5 * (x1 - xm1);
        let c2 = xm1 - 2.5 * x0 + 2.0 * x1 - 0.5 * x2;
        let c3 = 0.5 * (x2 - xm1) + 1.5 * (x0 - x1);
        ((c3 * t + c2) * t + c1) * t + x0
    }
}

/// Passe-tout de Schroeder : diffuse sans colorer le spectre.
#[derive(Clone, Debug)]
pub struct Allpass {
    line: DelayLine,
    delay: usize,
    pub gain: f32,
}

impl Allpass {
    /// Alloue : à appeler hors du thread audio.
    pub fn new(delay: usize, gain: f32) -> Self {
        Self { line: DelayLine::new(delay), delay: delay.max(1), gain }
    }

    pub fn clear(&mut self) {
        self.line.clear();
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let delayed = self.line.read(self.delay);
        let v = x + self.gain * delayed;
        self.line.write(v);
        delayed - self.gain * v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_and_fractional_reads() {
        let mut d = DelayLine::new(100);
        for n in 0..50 {
            d.write(n as f32);
        }
        assert_eq!(d.read(1), 49.0);
        assert_eq!(d.read(10), 40.0);
        assert!((d.read_frac(10.0) - 40.0).abs() < 1e-5);
        assert!((d.read_frac(10.25) - 39.75).abs() < 1e-4);
    }

    #[test]
    fn allpass_keeps_energy() {
        let mut ap = Allpass::new(37, 0.6);
        let mut energy = 0.0;
        for n in 0..10_000 {
            let y = ap.process(if n == 0 { 1.0 } else { 0.0 });
            energy += y * y;
        }
        assert!((energy - 1.0f32).abs() < 1e-3, "{energy}");
    }
}
