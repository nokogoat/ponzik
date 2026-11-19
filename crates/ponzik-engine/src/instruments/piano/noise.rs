//! Mechanical noises: the hammer's knock and the key bottoming on the key
//! bed, the key coming back up, a damper landing on a ringing string, the
//! pedal and its damper rail. Each is a short burst of filtered noise with
//! its own color and envelope.

/// Bursts playing at once at most.
const EVENTS: usize = 24;

#[derive(Clone, Copy, Debug, Default)]
struct Event {
    active: bool,
    /// State variable filter: coefficients and states.
    g: f32,
    k: f32,
    s1: f32,
    s2: f32,
    lowpass: bool,
    /// Envelope: decaying amplitude minus a faster one (soft attack).
    env: f32,
    att: f32,
    decay: f32,
    att_decay: f32,
    pan_l: f32,
    pan_r: f32,
    /// Share sent to the strings (pedal noises excite them).
    to_strings: f32,
}

/// Color of a burst.
#[derive(Clone, Copy, Debug)]
pub struct Burst {
    pub hz: f32,
    pub q: f32,
    pub lowpass: bool,
    pub amp: f32,
    /// Attack and decay time constants, s.
    pub attack: f32,
    pub decay: f32,
    pub pan: (f32, f32),
    pub to_strings: f32,
}

pub struct Noises {
    events: [Event; EVENTS],
    seed: u32,
    sample_rate: f32,
}

impl Noises {
    pub fn new(sample_rate: f32) -> Self {
        Self { events: [Event::default(); EVENTS], seed: 0x1234_5679, sample_rate }
    }

    pub fn clear(&mut self) {
        for e in &mut self.events {
            e.active = false;
        }
    }

    pub fn is_active(&self) -> bool {
        self.events.iter().any(|e| e.active)
    }

    /// Starts a burst, in place of the quietest one when all are busy.
    pub fn trigger(&mut self, b: Burst) {
        if !b.amp.is_finite() || b.amp <= 1e-6 {
            return;
        }
        let sr = self.sample_rate;
        let slot = self.events.iter().position(|e| !e.active).unwrap_or_else(|| {
            (0..EVENTS).min_by(|&a, &c| self.events[a].env.total_cmp(&self.events[c].env)).unwrap_or(0)
        });
        let hz = b.hz.clamp(20.0, sr * 0.45);
        self.events[slot] = Event {
            active: true,
            g: (std::f32::consts::PI * hz / sr).tan(),
            k: 1.0 / b.q.max(0.3),
            s1: 0.0,
            s2: 0.0,
            lowpass: b.lowpass,
            env: b.amp,
            att: b.amp,
            decay: (-1.0 / (b.decay.max(1e-4) * sr)).exp(),
            att_decay: (-1.0 / (b.attack.max(1e-5) * sr)).exp(),
            pan_l: b.pan.0,
            pan_r: b.pan.1,
            to_strings: b.to_strings,
        };
    }

    /// Adds the bursts into the ears and into the feed of the strings.
    pub fn render(&mut self, left: &mut [f32], right: &mut [f32], strings: &mut [f32]) {
        for e in self.events.iter_mut().filter(|e| e.active) {
            let (g, k) = (e.g, e.k);
            let a1 = 1.0 / (1.0 + g * (g + k));
            for ((l, r), s) in left.iter_mut().zip(right.iter_mut()).zip(strings.iter_mut()) {
                self.seed ^= self.seed << 13;
                self.seed ^= self.seed >> 17;
                self.seed ^= self.seed << 5;
                let x = self.seed as f32 * (2.0 / u32::MAX as f32) - 1.0;
                // Trapezoidal SVF (Simper).
                let v3 = x - e.s2;
                let v1 = a1 * (e.s1 + g * v3);
                let v2 = e.s2 + g * v1;
                e.s1 = 2.0 * v1 - e.s1;
                e.s2 = 2.0 * v2 - e.s2;
                let y = if e.lowpass { v2 } else { v1 * k };
                let out = y * (e.env - e.att);
                e.env *= e.decay;
                e.att *= e.att_decay;
                *l += out * e.pan_l;
                *r += out * e.pan_r;
                *s += out * e.to_strings;
            }
            if e.env < 1e-6 {
                e.active = false;
            }
        }
    }
}
