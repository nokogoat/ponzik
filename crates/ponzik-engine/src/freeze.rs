//! Freeze: a channel or a mixer track plays its offline render instead of its
//! devices, which are unloaded to save CPU.
//!
//! The render starts at the very beginning of the song timeline (second 0)
//! and holds the tail. A frozen channel plays it in place of its instrument
//! (its fader, pan and note routing stay live); a frozen track plays it in
//! place of its input and inserts, just before its fader. Taps capture those
//! very signals while rendering, so that the frozen version sounds the same.

use std::sync::Arc;

use crate::sample::Sample;

/// What the transport does in the current block, for the frozen players.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FreezeClock {
    pub playing: bool,
    /// The song timeline plays (song mode).
    pub song: bool,
    /// The block follows the previous one without a jump.
    pub contiguous: bool,
    /// Song time at the start of the block, in seconds.
    pub secs: f64,
    pub sample_rate: f64,
}

/// Player of a frozen render, synchronized to the song timeline.
#[derive(Clone, Debug)]
pub struct Frozen {
    pub sample: Arc<Sample>,
    /// Position in the render, in its frames.
    pos: f64,
    /// Render frames per output sample.
    step: f64,
    active: bool,
}

impl Frozen {
    pub fn new(sample: Arc<Sample>) -> Self {
        Self { sample, pos: 0.0, step: 1.0, active: false }
    }

    /// Follows the transport at the start of a block: carries on after the
    /// previous block, starts over from the song position after a jump, and
    /// stays silent outside the song (stopped, pattern mode). Once started, it
    /// carries on as long as the transport runs without a jump, even outside
    /// song mode (the tail of an export).
    pub(crate) fn sync(&mut self, clock: &FreezeClock) {
        let rate = self.sample.sample_rate.max(1.0) as f64;
        self.step = rate / clock.sample_rate.max(1.0);
        if !clock.playing {
            self.active = false;
        } else if !(self.active && clock.contiguous) {
            self.active = clock.song;
            let at = clock.secs.max(0.0) * rate;
            // Same rate: on the frame, so that the render is copied exactly.
            self.pos = if (self.step - 1.0).abs() < 1e-12 { at.round() } else { at };
        }
    }

    /// Writes the next `l.len()` frames over `l` and `r` (silence when stopped
    /// or past the end).
    pub(crate) fn read(&mut self, l: &mut [f32], r: &mut [f32]) {
        let s = &*self.sample;
        let len = s.len();
        if !self.active || len == 0 {
            l.fill(0.0);
            r.fill(0.0);
            return;
        }
        let right = if s.right.len() == len { &s.right } else { &s.left };
        for (a, b) in l.iter_mut().zip(r.iter_mut()) {
            let p = self.pos;
            if p < 0.0 || p >= len as f64 {
                (*a, *b) = (0.0, 0.0);
            } else {
                let i = p as usize;
                let frac = (p - i as f64) as f32;
                if frac == 0.0 {
                    (*a, *b) = (s.left[i], right[i]);
                } else {
                    let j = (i + 1).min(len - 1);
                    *a = s.left[i] + (s.left[j] - s.left[i]) * frac;
                    *b = right[i] + (right[j] - right[i]) * frac;
                }
            }
            self.pos += self.step;
        }
    }
}

/// Signal captured while rendering offline (see [`crate::Engine::set_taps`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tap {
    /// What the channel's instrument (after its note effects) plays, before
    /// the channel's volume and pan.
    Channel(usize),
    /// What the channel sends to its mixer track (volume and pan applied).
    ChannelOut(usize),
    /// A mixer track after its inserts, before its fader; for the master,
    /// its input (the whole mix before the master inserts).
    Track(usize),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(n: usize, rate: f32) -> Arc<Sample> {
        let left: Vec<f32> = (0..n).map(|i| i as f32).collect();
        let right = left.iter().map(|v| -v).collect();
        Arc::new(Sample { name: "r".into(), sample_rate: rate, left, right, markers: Vec::new() })
    }

    fn clock(playing: bool, song: bool, contiguous: bool, secs: f64) -> FreezeClock {
        FreezeClock { playing, song, contiguous, secs, sample_rate: 100.0 }
    }

    #[test]
    fn follows_the_timeline_and_its_jumps() {
        let mut f = Frozen::new(ramp(50, 100.0));
        let (mut l, mut r) = ([9.0; 4], [9.0; 4]);
        f.sync(&clock(false, true, false, 0.0));
        f.read(&mut l, &mut r);
        assert_eq!(l, [0.0; 4], "stopped: silent");
        f.sync(&clock(true, true, false, 0.1));
        f.read(&mut l, &mut r);
        assert_eq!((l, r), ([10.0, 11.0, 12.0, 13.0], [-10.0, -11.0, -12.0, -13.0]));
        // Next block: carries on, whatever the clock says.
        f.sync(&clock(true, false, true, 7.0));
        f.read(&mut l, &mut r);
        assert_eq!(l, [14.0, 15.0, 16.0, 17.0]);
        // A jump outside song mode: silent; back in the song at 0.48 s.
        f.sync(&clock(true, false, false, 0.0));
        f.read(&mut l, &mut r);
        assert_eq!(l, [0.0; 4]);
        f.sync(&clock(true, true, false, 0.48));
        f.read(&mut l, &mut r);
        assert_eq!(l, [48.0, 49.0, 0.0, 0.0], "past the end: silence");
    }

    #[test]
    fn another_rate_is_interpolated() {
        let mut f = Frozen::new(ramp(50, 50.0));
        let (mut l, mut r) = ([0.0; 4], [0.0; 4]);
        f.sync(&clock(true, true, false, 0.1));
        f.read(&mut l, &mut r);
        assert_eq!(l, [5.0, 5.5, 6.0, 6.5]);
    }
}
