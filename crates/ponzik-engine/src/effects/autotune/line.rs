//! The input, kept for the shifters: left, right and their mix, read at any
//! fractional position with a four-point (Catmull-Rom) interpolation.

pub struct Line {
    buf: [Vec<f32>; 3],
    mask: usize,
}

/// Channel of the mono mix.
pub const MONO: usize = 2;

impl Line {
    /// `len`: samples kept (rounded up to a power of two).
    pub fn new(len: usize) -> Self {
        let n = len.next_power_of_two().max(64);
        Self { buf: [vec![0.0; n], vec![0.0; n], vec![0.0; n]], mask: n - 1 }
    }

    pub fn clear(&mut self) {
        self.buf.iter_mut().for_each(|b| b.fill(0.0));
    }

    #[inline]
    pub fn write(&mut self, at: u64, l: f32, r: f32) {
        let i = at as usize & self.mask;
        self.buf[0][i] = l;
        self.buf[1][i] = r;
        self.buf[MONO][i] = 0.5 * (l + r);
    }

    #[inline]
    pub fn get(&self, ch: usize, at: i64) -> f32 {
        if at < 0 { 0.0 } else { self.buf[ch][at as usize & self.mask] }
    }

    #[inline]
    pub fn interp(&self, ch: usize, pos: f64) -> f32 {
        let i = pos.floor();
        let f = (pos - i) as f32;
        let i = i as i64;
        let (y0, y1, y2, y3) = (self.get(ch, i - 1), self.get(ch, i), self.get(ch, i + 1), self.get(ch, i + 2));
        let c1 = 0.5 * (y2 - y0);
        let c2 = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3;
        let c3 = 0.5 * (y3 - y0) + 1.5 * (y1 - y2);
        ((c3 * f + c2) * f + c1) * f + y1
    }
}
