//! The recent detection frames with their correction, looked up by time by
//! the shifters (the frame nearest a pitch mark, the shift between two frames).

/// One frame as the shifters see it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    /// Center of the analysis, absolute input samples.
    pub time: f64,
    pub hz: f32,
    pub voiced: bool,
    /// Semitones to move the voice by.
    pub shift: f32,
}

/// Ring of the last frames (about a second).
pub struct Frames {
    buf: Vec<Point>,
    count: usize,
}

const CAP: usize = 512;

impl Frames {
    pub fn new() -> Self {
        Self { buf: vec![Point::default(); CAP], count: 0 }
    }

    pub fn clear(&mut self) {
        self.count = 0;
    }

    pub fn push(&mut self, p: Point) {
        self.buf[self.count % CAP] = p;
        self.count += 1;
    }

    fn get(&self, k: usize) -> Point {
        self.buf[k % CAP]
    }

    pub fn latest(&self) -> Option<Point> {
        self.count.checked_sub(1).map(|k| self.get(k))
    }

    /// Index of the last frame at or before `t` (the first one kept if all are later).
    fn before(&self, t: f64) -> Option<usize> {
        let newest = self.count.checked_sub(1)?;
        let oldest = self.count.saturating_sub(CAP);
        let mut k = newest;
        while k > oldest && self.get(k).time > t {
            k -= 1;
        }
        Some(k)
    }

    /// Frame nearest `t`.
    pub fn at(&self, t: f64) -> Option<Point> {
        let k = self.before(t)?;
        let a = self.get(k);
        if k + 1 < self.count {
            let b = self.get(k + 1);
            if (b.time - t).abs() < (t - a.time).abs() {
                return Some(b);
            }
        }
        Some(a)
    }

    /// Shift at `t`, linear between frames (held past the newest one).
    pub fn shift_at(&self, t: f64) -> f32 {
        let Some(k) = self.before(t) else { return 0.0 };
        let a = self.get(k);
        if k + 1 >= self.count || t <= a.time {
            return a.shift;
        }
        let b = self.get(k + 1);
        let f = ((t - a.time) / (b.time - a.time).max(1e-9)).clamp(0.0, 1.0) as f32;
        a.shift + (b.shift - a.shift) * f
    }
}

impl Default for Frames {
    fn default() -> Self {
        Self::new()
    }
}
