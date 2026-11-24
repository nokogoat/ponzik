//! Crossfader and cue, as in Ableton Live's mixer.
//!
//! Each track can be assigned to side A or side B of the crossfader (or to
//! neither: it ignores it). The crossfader position goes from 0 (only A) to
//! 1 (only B); its curve decides how the two sides trade places. The gains
//! are computed on the interface thread ([`gains`]) and sent to the mixer,
//! which ramps them over each block so a fast move does not click.
//!
//! Cue: tracks whose cue is on are summed, post-fader and before the
//! crossfader, into a cue bus that never reaches the master. The audio
//! backend opens one stereo pair only, so the cue bus is heard by blending it
//! into what the device plays ([`Mixer::heard`](crate::mixer::Mixer)): the
//! master itself, its meters, recordings and exports stay the full mix.

/// Side of the crossfader a track listens to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Side {
    /// The track ignores the crossfader.
    #[default]
    None,
    A,
    B,
}

impl Side {
    pub const ALL: [Side; 3] = [Side::None, Side::A, Side::B];

    /// Index in the mixer's gain table (0: unity).
    pub fn index(self) -> usize {
        match self {
            Side::None => 0,
            Side::A => 1,
            Side::B => 2,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Side::None => "none",
            Side::A => "a",
            Side::B => "b",
        }
    }

    pub fn from_id(id: &str) -> Option<Side> {
        Self::ALL.into_iter().find(|s| s.id() == id)
    }
}

/// How the two sides trade places along the crossfader.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Curve {
    /// Gains fall in straight lines: −6 dB each in the middle.
    Linear,
    /// Sine and cosine: −3 dB each in the middle, the power stays constant.
    #[default]
    Power,
    /// Both sides at full level until the last few percent (scratching).
    Cut,
}

/// Width of the fade at each end of a [`Curve::Cut`] crossfader.
pub const CUT_WIDTH: f32 = 0.05;

impl Curve {
    pub const ALL: [Curve; 3] = [Curve::Linear, Curve::Power, Curve::Cut];

    pub fn id(self) -> &'static str {
        match self {
            Curve::Linear => "linear",
            Curve::Power => "power",
            Curve::Cut => "cut",
        }
    }

    pub fn from_id(id: &str) -> Option<Curve> {
        Self::ALL.into_iter().find(|c| c.id() == id)
    }
}

/// Gains of side A and side B with the crossfader at `pos` (0: A, 1: B).
pub fn gains(pos: f32, curve: Curve) -> (f32, f32) {
    let x = if pos.is_finite() { pos.clamp(0.0, 1.0) } else { 0.5 };
    match curve {
        Curve::Linear => (1.0 - x, x),
        Curve::Power => {
            let a = x * std::f32::consts::FRAC_PI_2;
            // Exact at the ends: the far side is silent, not almost.
            let snap = |g: f32| if g < 1e-6 { 0.0 } else { g.min(1.0) };
            (snap(a.cos()), snap(a.sin()))
        }
        Curve::Cut => (((1.0 - x) / CUT_WIDTH).min(1.0), (x / CUT_WIDTH).min(1.0)),
    }
}

/// Gain table of the mixer for a crossfader: unity, side A, side B.
pub fn table(pos: f32, curve: Curve) -> [f32; 3] {
    let (a, b) = gains(pos, curve);
    [1.0, a, b]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    #[test]
    fn linear_falls_in_straight_lines() {
        assert_eq!(gains(0.0, Curve::Linear), (1.0, 0.0));
        assert_eq!(gains(1.0, Curve::Linear), (0.0, 1.0));
        assert_eq!(gains(0.5, Curve::Linear), (0.5, 0.5));
        let (a, b) = gains(0.25, Curve::Linear);
        assert!(near(a, 0.75) && near(b, 0.25));
        // Amplitudes sum to one all along.
        assert!((0..=20).all(|k| {
            let (a, b) = gains(k as f32 / 20.0, Curve::Linear);
            near(a + b, 1.0)
        }));
    }

    #[test]
    fn power_keeps_the_power_constant() {
        assert_eq!(gains(0.0, Curve::Power), (1.0, 0.0));
        assert_eq!(gains(1.0, Curve::Power), (0.0, 1.0));
        let (a, b) = gains(0.5, Curve::Power);
        // −3 dB each in the middle.
        assert!(near(a, std::f32::consts::FRAC_1_SQRT_2) && near(b, a));
        assert!((0..=20).all(|k| {
            let (a, b) = gains(k as f32 / 20.0, Curve::Power);
            near(a * a + b * b, 1.0)
        }));
        // Monotonic: A only falls, B only rises.
        let pts: Vec<(f32, f32)> = (0..=50).map(|k| gains(k as f32 / 50.0, Curve::Power)).collect();
        assert!(pts.windows(2).all(|w| w[1].0 <= w[0].0 && w[1].1 >= w[0].1));
    }

    #[test]
    fn cut_keeps_both_sides_until_the_ends() {
        assert_eq!(gains(0.5, Curve::Cut), (1.0, 1.0));
        assert_eq!(gains(0.1, Curve::Cut), (1.0, 1.0));
        assert_eq!(gains(0.9, Curve::Cut), (1.0, 1.0));
        assert_eq!(gains(0.0, Curve::Cut), (1.0, 0.0));
        assert_eq!(gains(1.0, Curve::Cut), (0.0, 1.0));
        let (a, b) = gains(CUT_WIDTH * 0.5, Curve::Cut);
        assert!(near(a, 1.0) && near(b, 0.5));
    }

    #[test]
    fn out_of_range_positions_are_clamped() {
        for c in Curve::ALL {
            assert_eq!(gains(-3.0, c), gains(0.0, c));
            assert_eq!(gains(7.0, c), gains(1.0, c));
            assert_eq!(gains(f32::NAN, c), gains(0.5, c));
            assert_eq!(Curve::from_id(c.id()), Some(c));
        }
        assert_eq!(table(0.0, Curve::Linear), [1.0, 1.0, 0.0]);
        for s in Side::ALL {
            assert_eq!(Side::from_id(s.id()), Some(s));
        }
    }
}
