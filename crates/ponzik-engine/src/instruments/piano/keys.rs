//! Physical description of each key of the piano: strings, hammer, decay
//! times, inharmonicity and the stretched tuning that follows from it.
//!
//! Values are drawn from published measurements of grand pianos (Chaigne and
//! Askenfelt for the hammers, Fletcher and Conklin for the inharmonicity and
//! the scale) and interpolated across the keyboard; `size` turns the concert
//! grand into an upright with shorter, stiffer bass strings.

/// First and last key of the keyboard (A0 and C8).
pub const LOWEST: u8 = 21;
pub const HIGHEST: u8 = 108;
pub const KEYS: usize = (HIGHEST - LOWEST + 1) as usize;
/// The top 18 keys have no dampers: their short strings ring out freely.
pub const FIRST_UNDAMPED: u8 = HIGHEST - 17;

/// Linear interpolation in a table of `(key, value)` points, constant beyond
/// its ends.
fn interp(table: &[(f32, f32)], key: f32) -> f32 {
    let (first, last) = (table[0], table[table.len() - 1]);
    if key <= first.0 {
        return first.1;
    }
    if key >= last.0 {
        return last.1;
    }
    for w in table.windows(2) {
        let ((k0, v0), (k1, v1)) = (w[0], w[1]);
        if key <= k1 {
            return v0 + (v1 - v0) * (key - k0) / (k1 - k0);
        }
    }
    last.1
}

/// Same, between logarithms of the values.
fn interp_log(table: &[(f32, f32)], key: f32) -> f32 {
    let mut logs = [(0.0f32, 0.0f32); 12];
    for (o, &(k, v)) in logs.iter_mut().zip(table) {
        *o = (k, v.ln());
    }
    interp(&logs[..table.len().min(12)], key).exp()
}

/// Inharmonicity coefficient of a concert grand (Railsback curve: lowest
/// near the bass break, steep rise in the treble).
const INHARMONICITY: [(f32, f32); 8] = [
    (21.0, 2.6e-4),
    (33.0, 1.3e-4),
    (45.0, 1.6e-4),
    (60.0, 4.0e-4),
    (72.0, 9.0e-4),
    (84.0, 2.4e-3),
    (96.0, 6.0e-3),
    (108.0, 1.5e-2),
];

/// T60 of the fundamental's aftersound (slow decay), seconds.
const AFTERSOUND_T60: [(f32, f32); 8] = [
    (21.0, 40.0),
    (33.0, 34.0),
    (45.0, 26.0),
    (57.0, 19.0),
    (69.0, 12.0),
    (81.0, 6.5),
    (93.0, 3.2),
    (108.0, 1.3),
];

/// T60 of a string once its damper rests on it, seconds: bass dampers grab
/// heavy strings less firmly.
const DAMPED_T60: [(f32, f32); 5] = [(21.0, 0.9), (33.0, 0.6), (45.0, 0.35), (69.0, 0.18), (90.0, 0.1)];

/// Hammers: (key, mass kg, felt stiffness K, exponent p), F = K·δ^p.
const HAMMERS: [(f32, f32, f32, f32); 6] = [
    (21.0, 6.5e-3, 1.5e8, 2.2),
    (36.0, 4.9e-3, 4.0e8, 2.3),
    (60.0, 2.97e-3, 4.5e9, 2.5),
    (80.0, 2.5e-3, 1.2e11, 2.8),
    (96.0, 1.8e-3, 5.0e12, 3.0),
    (108.0, 1.2e-3, 4.0e13, 3.1),
];

/// Pseudo-random value in [−1, 1] for a key and a salt (fixed spread of the
/// unison strings, hammer variations).
pub fn hash(key: u8, salt: u32) -> f32 {
    let mut x = (key as u32).wrapping_mul(0x9E37_79B9) ^ salt.wrapping_mul(0x85EB_CA6B);
    x ^= x >> 15;
    x = x.wrapping_mul(0x2C1B_3C6D);
    x ^= x >> 12;
    x = x.wrapping_mul(0x297A_2D39);
    x ^= x >> 15;
    (x as f32 / u32::MAX as f32) * 2.0 - 1.0
}

/// Strings struck by one hammer: one in the low bass, two up to C3, three
/// above.
pub fn strings(key: u8) -> usize {
    match key {
        0..=28 => 1,
        29..=48 => 2,
        _ => 3,
    }
}

/// Speaking length of the strings in metres: about 1.87× per octave in the
/// treble, capped in the bass by the case (2 m concert grand, 1.2 m upright).
pub fn length(key: u8, size: f32) -> f32 {
    let f = 440.0 * ((key as f32 - 69.0) / 12.0).exp2();
    let max = 1.2 + 0.8 * size.clamp(0.0, 1.0);
    let natural = 0.62 * (261.63 / f).powf(0.9);
    // Smooth cap.
    natural * max / (natural.powi(4) + max.powi(4)).powf(0.25)
}

/// Inharmonicity coefficient B of the strings of `key`. B grows as 1/L²:
/// the short bass strings of an upright are much stiffer.
pub fn inharmonicity(key: u8, size: f32) -> f32 {
    let b = interp_log(&INHARMONICITY, key as f32);
    let ratio = length(key, 1.0) / length(key, size);
    b * ratio * ratio
}

/// String tension in newtons.
pub fn tension(key: u8) -> f32 {
    interp(&[(21.0, 780.0), (60.0, 700.0), (108.0, 640.0)], key as f32)
}

pub fn aftersound_t60(key: u8) -> f32 {
    interp_log(&AFTERSOUND_T60, key as f32)
}

/// Prompt sound T60 over aftersound T60: the two-stage decay is clearest in
/// the middle.
pub fn prompt_ratio(key: u8) -> f32 {
    interp(&[(21.0, 0.3), (45.0, 0.2), (69.0, 0.22), (96.0, 0.4), (108.0, 0.6)], key as f32)
}

pub fn damped_t60(key: u8) -> f32 {
    interp_log(&DAMPED_T60, key as f32)
}

/// Hammer of `key`: (mass kg, stiffness K, exponent p).
pub fn hammer(key: u8) -> (f32, f32, f32) {
    let k = key as f32;
    let mass = interp(&HAMMERS.map(|h| (h.0, h.1)), k);
    let stiffness = interp_log(&HAMMERS.map(|h| (h.0, h.2)), k);
    let p = interp(&HAMMERS.map(|h| (h.0, h.3)), k);
    (mass, stiffness, p)
}

/// Strike point as a fraction of the string length, from the agraffe:
/// about 1/8, closer to the end in the high treble.
pub fn strike_point(key: u8) -> f32 {
    interp(&[(21.0, 0.122), (60.0, 0.12), (84.0, 0.105), (108.0, 0.08)], key as f32) * (1.0 + 0.03 * hash(key, 7))
}

/// Ratio of partial `n` (from 1) over the first partial for a stiff string.
pub fn partial_ratio(n: f32, b: f32) -> f32 {
    n * ((1.0 + b * n * n) / (1.0 + b)).sqrt()
}

/// Stretch of octave `upper − 12 → upper` in cents when partial `2a` of the
/// lower note is tuned beatless to partial `a` of the upper one.
fn octave_stretch(lower: u8, upper: u8, a: f32, size: f32) -> f32 {
    let (bl, bu) = (inharmonicity(lower, size), inharmonicity(upper, size));
    let ratio = partial_ratio(2.0 * a, bl) / (2.0 * partial_ratio(a, bu));
    1200.0 * ratio.log2()
}

/// Partial of the upper note matched by the tuner in each register: higher
/// partials in the bass (6:3, 10:5), plain octaves in the treble. Continuous
/// across the keyboard so that the curve has no steps.
fn matched_partial(upper: u8) -> f32 {
    interp(&[(33.0, 5.0), (45.0, 3.0), (60.0, 2.0), (84.0, 1.5), (96.0, 1.0)], upper as f32)
}

/// Deviation of each key from equal temperament in cents (Railsback stretch
/// that follows from the inharmonicity), A4 at 0. Index: key − [`LOWEST`].
pub fn stretch_curve(size: f32) -> [f32; KEYS] {
    let mut s = [0.0f32; KEYS];
    let idx = |k: u8| (k - LOWEST) as usize;
    // Temperament octave A3–A4: stretched like the octaves around it, spread
    // evenly over its semitones.
    let temperament = octave_stretch(57, 69, 2.0, size);
    for k in 57..=69u8 {
        s[idx(k)] = -temperament * (69 - k) as f32 / 12.0;
    }
    for k in 70..=HIGHEST {
        s[idx(k)] = s[idx(k - 12)] + octave_stretch(k - 12, k, matched_partial(k), size);
    }
    for k in (LOWEST..57).rev() {
        s[idx(k)] = s[idx(k + 12)] - octave_stretch(k, k + 12, matched_partial(k + 12), size);
    }
    // Smoothed over five keys, A4 kept at 0.
    let smooth: [f32; KEYS] = std::array::from_fn(|i| {
        let (a, b) = (i.saturating_sub(2), (i + 3).min(KEYS));
        s[a..b].iter().sum::<f32>() / (b - a) as f32
    });
    let a4 = smooth[idx(69)];
    smooth.map(|c| c - a4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_is_plausible() {
        assert!((length(21, 1.0) - 2.0).abs() < 0.25, "{}", length(21, 1.0));
        assert!((length(60, 1.0) - 0.62).abs() < 0.08, "{}", length(60, 1.0));
        assert!(length(108, 1.0) < 0.07);
        assert!(length(21, 0.0) < 1.3);
        // An upright's bass is stiffer; the treble is the same.
        assert!(inharmonicity(21, 0.0) > 2.0 * inharmonicity(21, 1.0));
        assert!((inharmonicity(100, 0.0) / inharmonicity(100, 1.0) - 1.0).abs() < 0.05);
        assert_eq!((strings(21), strings(40), strings(80)), (1, 2, 3));
    }

    #[test]
    fn stretch_follows_railsback() {
        let s = stretch_curve(1.0);
        let at = |k: u8| s[(k - LOWEST) as usize];
        assert_eq!(at(69), 0.0);
        // Flat bass, sharp treble, monotonic overall.
        assert!(at(21) < -8.0 && at(21) > -50.0, "A0 {}", at(21));
        assert!(at(108) > 10.0 && at(108) < 60.0, "C8 {}", at(108));
        assert!(at(45) < 0.0 && at(93) > 0.0);
        for k in LOWEST..HIGHEST {
            assert!(at(k + 1) >= at(k) - 0.2, "{k}");
        }
        // Stiffer strings stretch more.
        assert!(stretch_curve(0.0)[0] < at(21));
    }
}
