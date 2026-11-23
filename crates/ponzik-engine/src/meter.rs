use std::sync::atomic::{AtomicU32, Ordering};

/// Crête stéréo publiée par le thread audio. Les bits d'un `f32` positif sont
/// ordonnés comme des entiers : `fetch_max` suffit, sans verrou.
#[derive(Default)]
pub(crate) struct PeakMeter([AtomicU32; 2]);

impl PeakMeter {
    pub(crate) fn record(&self, l: f32, r: f32) {
        self.0[0].fetch_max(l.abs().to_bits(), Ordering::Relaxed);
        self.0[1].fetch_max(r.abs().to_bits(), Ordering::Relaxed);
    }

    pub(crate) fn take(&self) -> [f32; 2] {
        [0, 1].map(|i| f32::from_bits(self.0[i].swap(0, Ordering::Relaxed)))
    }
}

/// Largest magnitude of `buf` (NaN ignored). Eight running maxima the
/// compiler turns into SIMD compares: `f32::max` in a fold stays scalar.
pub(crate) fn peak(buf: &[f32]) -> f32 {
    let mut acc = [0.0f32; 8];
    let chunks = buf.chunks_exact(8);
    let rest = chunks.remainder();
    for c in chunks {
        for (a, &x) in acc.iter_mut().zip(c) {
            let x = x.abs();
            *a = if x > *a { x } else { *a };
        }
    }
    let mut m = acc.iter().fold(0.0f32, |m, &a| if a > m { a } else { m });
    for &x in rest {
        let x = x.abs();
        m = if x > m { x } else { m };
    }
    m
}

#[cfg(test)]
mod tests {
    use super::peak;

    #[test]
    fn peak_finds_the_largest_magnitude_anywhere_and_ignores_nan() {
        assert_eq!(peak(&[]), 0.0);
        for len in [1, 7, 8, 9, 31, 240] {
            for at in [0, len / 2, len - 1] {
                let mut v = vec![0.1f32; len];
                v[at] = -0.75;
                assert_eq!(peak(&v), 0.75, "len {len}, at {at}");
                v[at] = f32::NAN;
                let rest = if len > 1 { 0.1 } else { 0.0 };
                assert_eq!(peak(&v), rest, "NaN ignored, len {len}");
            }
        }
    }
}
