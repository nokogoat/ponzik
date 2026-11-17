/// Ligne de retard circulaire, allouée une seule fois puis lue en position
/// fractionnaire (interpolation d'Hermite).
///
/// On lit avant d'écrire : `read(d)` rend l'échantillon écrit il y a `d` pas.
#[derive(Clone, Debug)]
pub struct DelayLine {
    buf: Vec<f32>,
    /// Anneau en service : `mask + 1` cases (puissance de deux) au début de `buf`.
    mask: usize,
    pos: usize,
    /// Plus grand retard lisible dans l'anneau en service.
    max: usize,
    /// Plus grand retard demandé à la création.
    limit: usize,
    /// Anneau plus grand à prendre au prochain tour (0 : aucun).
    pending: usize,
}

/// Anneau de départ d'une ligne extensible, en échantillons.
const GROWABLE_START: usize = 4_096;

impl DelayLine {
    /// Alloue : à appeler hors du thread audio.
    pub fn new(max_delay: usize) -> Self {
        let len = (max_delay + 4).next_power_of_two();
        let max = max_delay.max(2);
        Self { buf: vec![0.0; len], mask: len - 1, pos: 0, max, limit: max, pending: 0 }
    }

    /// Ligne qui réserve `max_delay` sans le toucher : son anneau commence
    /// petit et grandit (sans allocation) quand [`DelayLine::reserve`] demande
    /// un retard plus long. Les pages jamais écrites ne coûtent pas de mémoire :
    /// un écho de 250 ms n'occupe pas les secondes que la ligne pourrait tenir.
    pub fn growable(max_delay: usize) -> Self {
        let len = (max_delay + 4).next_power_of_two();
        let ring = GROWABLE_START.min(len);
        let max = max_delay.max(2);
        Self { buf: vec![0.0; len], mask: ring - 1, pos: 0, max: max.min(ring - 4).max(2), limit: max, pending: 0 }
    }

    pub fn max_delay(&self) -> usize {
        self.max
    }

    /// Prépare un retard de `delay` échantillons (borné à la réserve). Si
    /// l'anneau est trop court, il grandit quand son curseur repasse par le
    /// début : l'historique garde alors sa place et rien n'est recopié.
    /// Jusque-là, les lectures restent bornées à l'anneau actuel.
    pub fn reserve(&mut self, delay: usize) {
        let need = (delay.min(self.limit) + 4).next_power_of_two().min(self.buf.len());
        if need > self.mask + 1 {
            self.pending = self.pending.max(need);
        }
    }

    pub fn clear(&mut self) {
        // Seul l'anneau en service a pu être écrit.
        self.buf[..=self.mask].fill(0.0);
    }

    #[inline]
    pub fn write(&mut self, x: f32) {
        self.buf[self.pos] = x;
        self.pos = (self.pos + 1) & self.mask;
        if self.pos == 0 && self.pending > 0 {
            self.grow();
        }
    }

    /// Passe à l'anneau demandé, curseur au début de l'anneau précédent : les
    /// cases déjà écrites gardent leur âge, les nouvelles (jamais écrites) se
    /// lisent comme du silence.
    #[cold]
    fn grow(&mut self) {
        let old = self.mask + 1;
        self.mask = self.pending - 1;
        self.pos = old;
        self.max = self.limit.min(self.pending - 4).max(2);
        self.pending = 0;
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
    fn growable_line_keeps_its_history_when_it_grows() {
        let mut fixed = DelayLine::new(100_000);
        let mut grow = DelayLine::growable(100_000);
        assert_eq!(grow.max_delay(), GROWABLE_START - 4);
        let mut n = 0.0f32;
        let mut step = |f: &mut DelayLine, g: &mut DelayLine, k: usize| {
            for _ in 0..k {
                n += 1.0;
                f.write(n);
                g.write(n);
            }
        };
        step(&mut fixed, &mut grow, 5_000);
        grow.reserve(30_000);
        // Not yet: the ring grows when its cursor wraps.
        assert_eq!(grow.max_delay(), GROWABLE_START - 4);
        step(&mut fixed, &mut grow, 4_000);
        assert_eq!(grow.max_delay(), 32_768 - 4);
        // The last samples written read the same in both lines…
        for d in [1, 10, 999, 4_000, GROWABLE_START] {
            assert_eq!(grow.read(d), fixed.read(d), "delay {d}");
        }
        // …older ones, from before the growth, are silence for a moment…
        assert_eq!(grow.read(20_000), 0.0);
        // …and once the new ring is full, everything matches again.
        step(&mut fixed, &mut grow, 40_000);
        for d in [1, 5_000, 20_000, 32_000] {
            assert_eq!(grow.read(d), fixed.read(d), "delay {d}");
            assert_eq!(grow.read_frac(d as f32 + 0.5), fixed.read_frac(d as f32 + 0.5));
        }
        // Bounded by the reserve; clearing keeps it silent.
        grow.reserve(1 << 30);
        step(&mut fixed, &mut grow, 40_000);
        assert_eq!(grow.max_delay(), 100_000);
        grow.clear();
        assert_eq!(grow.read(50_000), 0.0);
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
