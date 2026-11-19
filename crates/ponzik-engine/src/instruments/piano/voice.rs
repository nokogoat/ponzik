//! One key: its strings as a bank of damped modes, struck by a nonlinear
//! felt hammer.
//!
//! Each partial n of the stiff strings vibrates at f₁·n·√((1 + B·n²)/(1 + B)).
//! The unison strings of a key, coupled through the bridge, share their
//! energy between a symmetric mode (strings in phase: strong coupling to the
//! soundboard, fast decay, the "prompt sound") and antisymmetric ones (strings
//! against each other: weak coupling, slow decay, the "aftersound"), slightly
//! apart in frequency from the mistuning of the strings: the two-stage decay
//! and the beating of a real piano. Single bass strings get the same from
//! their two polarizations.
//!
//! Every mode is a complex phasor `z` turning by `c = e^{(−σ+iω)/fs}` each
//! sample; its displacement is Im z. The hammer is a mass on a felt spring
//! F = K·δ^p (δ: felt compression), solved implicitly against the string
//! displacement at the strike point each sample, so the contact duration and
//! the spectrum it leaves follow the velocity: a harder blow compresses the
//! stiffening felt further, the contact is shorter and the tone brighter.
//! Once the hammer has left, the voice only turns its phasors (eight at a
//! time, SIMD friendly) and drops the partials that fell under audibility.

use std::f32::consts::{PI, TAU};

/// Modes of a voice at most, a multiple of [`LANES`].
pub const MAX_MODES: usize = 192;
/// Modes processed together.
pub const LANES: usize = 8;
/// Samples rendered per block.
pub const BLOCK: usize = 64;
/// Partials with slow (aftersound) companions.
const SLOW_PARTIALS: usize = 40;
/// No partial above this frequency.
const MAX_PARTIAL_HZ: f32 = 16_000.0;
/// A contact lasts this long at most (seconds).
const MAX_CONTACT: f32 = 0.015;
/// The hammer is gone after this long without touching the string.
const RELEASE_GAP: f32 = 0.002;

/// What vibrates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Material {
    /// Steel strings (piano).
    #[default]
    Strings,
    /// Metal rods clamped at one end (toy piano).
    Rods,
    /// Free metal bars over resonators (celesta).
    Bars,
}

/// Mode ratios of a clamped-free rod and of a free-free bar.
const ROD_RATIOS: [f32; 6] = [1.0, 6.267, 17.547, 34.386, 56.843, 84.913];
const BAR_RATIOS: [f32; 6] = [1.0, 2.756, 5.404, 8.933, 13.344, 18.638];

/// Everything a strike needs, worked out by the instrument from the key, the
/// settings and the pedals.
#[derive(Clone, Copy, Debug)]
pub struct Setup {
    pub sample_rate: f32,
    pub material: Material,
    /// First partial, Hz.
    pub f1: f32,
    /// Inharmonicity coefficient.
    pub b: f32,
    pub strings: usize,
    /// Mistuning of each string (or polarization) in cents.
    pub detune: [f32; 3],
    /// T60 of the fundamental's aftersound, seconds.
    pub t60: f32,
    /// Prompt T60 over aftersound T60.
    pub prompt_ratio: f32,
    /// Extra damping rate (1/s) at 2 kHz, growing as f² (none on the
    /// fundamental).
    pub hf_loss: f32,
    /// T60 of the fundamental with the damper down.
    pub damped_t60: f32,
    /// Strike point over the string length.
    pub strike: f32,
    /// Excitation of the antisymmetric modes by the hammer (0: perfectly
    /// even strings), and their radiation.
    pub asym: f32,
    pub slow_out: f32,
    /// Excitation of the symmetric modes.
    pub sym: f32,
    /// Modal mass of the strings, kg.
    pub modal_mass: f32,
    pub hammer_mass: f32,
    pub felt_k: f32,
    pub felt_p: f32,
    /// Hammer velocity, m/s.
    pub velocity: f32,
}

/// One sounding key.
pub struct Voice {
    pub key: u8,
    pub active: bool,
    /// Key held down.
    pub held: bool,
    /// Order of the strikes, for stealing.
    pub age: u64,
    /// Mean power of the voice before its gain, updated every block.
    pub level: f32,
    /// Output gain of the key (calibration).
    pub gain: f32,
    /// Damper on the strings: 0 lifted, 1 resting.
    pub damper: f32,
    /// Modes in use (a multiple of [`LANES`]) and modes still audible.
    n: usize,
    live: usize,
    zr: [f32; MAX_MODES],
    zi: [f32; MAX_MODES],
    /// Rotation with the damper, and without.
    cr: [f32; MAX_MODES],
    ci: [f32; MAX_MODES],
    br: [f32; MAX_MODES],
    bi: [f32; MAX_MODES],
    /// Damper decay rate × sample period.
    dmp: [f32; MAX_MODES],
    /// Mode shape at the strike point (hammer coupling).
    hin: [f32; MAX_MODES],
    /// Response to a unit force held one sample.
    inr: [f32; MAX_MODES],
    ini: [f32; MAX_MODES],
    /// Radiation through the bridge.
    w: [f32; MAX_MODES],
    hammer: Hammer,
}

#[derive(Clone, Copy, Debug, Default)]
struct Hammer {
    touching: bool,
    /// Position and velocity, m and m/s; the string side is positive.
    y: f32,
    v: f32,
    mass: f32,
    k: f32,
    p: f32,
    /// dt²/2m + string compliance over one sample.
    compliance: f32,
    dt: f32,
    /// Samples since the strike, and since the last touch.
    t: u32,
    apart: u32,
    max_t: u32,
    gap: u32,
    contacts: bool,
}

impl Default for Voice {
    fn default() -> Self {
        Self {
            key: 0,
            active: false,
            held: false,
            age: 0,
            level: 0.0,
            gain: 1.0,
            damper: 0.0,
            n: 0,
            live: 0,
            zr: [0.0; MAX_MODES],
            zi: [0.0; MAX_MODES],
            cr: [0.0; MAX_MODES],
            ci: [0.0; MAX_MODES],
            br: [0.0; MAX_MODES],
            bi: [0.0; MAX_MODES],
            dmp: [0.0; MAX_MODES],
            hin: [0.0; MAX_MODES],
            inr: [0.0; MAX_MODES],
            ini: [0.0; MAX_MODES],
            w: [0.0; MAX_MODES],
            hammer: Hammer::default(),
        }
    }
}

/// One mode to lay out.
struct Mode {
    hz: f32,
    sigma: f32,
    damper_sigma: f32,
    shape: f32,
    weight: f32,
}

impl Voice {
    pub fn is_striking(&self) -> bool {
        self.active && self.hammer.touching
    }

    /// Strikes the key: lays out its modes (a sounding key keeps the motion of
    /// its strings: the hammer hits them as they move) and starts the hammer.
    pub fn strike(&mut self, key: u8, s: &Setup, age: u64, gain: f32) {
        let restrike = self.active && self.key == key;
        if restrike {
            if !self.hammer.touching {
                // Back to displacements.
                for i in 0..self.n {
                    let w = self.w[i];
                    if w != 0.0 {
                        self.zr[i] /= w;
                        self.zi[i] /= w;
                    }
                }
            }
        } else {
            self.zr = [0.0; MAX_MODES];
            self.zi = [0.0; MAX_MODES];
        }
        self.key = key;
        self.active = true;
        self.held = true;
        self.age = age;
        self.gain = gain;
        self.damper = 0.0;
        let sr = s.sample_rate;
        let dt = 1.0 / sr;
        let mut count = 0;
        let mut compliance = 0.0;
        let max_hz = (0.45 * sr).min(MAX_PARTIAL_HZ);
        layout(s, max_hz, |m| {
            if count >= MAX_MODES {
                return false;
            }
            let i = count;
            let w = TAU * m.hz;
            let r = (-m.sigma * dt).exp();
            let (sin, cos) = (w * dt).sin_cos();
            let (cr, ci) = (r * cos, r * sin);
            self.br[i] = cr;
            self.bi[i] = ci;
            self.cr[i] = cr;
            self.ci[i] = ci;
            self.dmp[i] = m.damper_sigma * dt;
            self.hin[i] = m.shape;
            // β = shape/(Mω) · (c − 1)/(−σ + iω): response to a force held
            // over one sample.
            let (a, b) = (cr - 1.0, ci);
            let (sr_, si_) = (-m.sigma, w);
            let norm = m.shape / (s.modal_mass * w * (sr_ * sr_ + si_ * si_));
            self.inr[i] = (a * sr_ + b * si_) * norm;
            self.ini[i] = (b * sr_ - a * si_) * norm;
            compliance += m.shape * self.ini[i];
            self.w[i] = m.weight;
            count += 1;
            true
        });
        let n = count.div_ceil(LANES) * LANES;
        for i in count..n.max(self.n) {
            self.zr[i] = 0.0;
            self.zi[i] = 0.0;
            self.cr[i] = 0.0;
            self.ci[i] = 0.0;
            self.br[i] = 0.0;
            self.bi[i] = 0.0;
            self.dmp[i] = 0.0;
            self.hin[i] = 0.0;
            self.inr[i] = 0.0;
            self.ini[i] = 0.0;
            self.w[i] = 0.0;
        }
        self.n = n;
        self.live = n;
        // The hammer starts against the string, wherever it is.
        let y = (0..n).map(|i| self.hin[i] * self.zi[i]).sum::<f32>();
        self.hammer = Hammer {
            touching: true,
            y,
            v: s.velocity.max(0.01),
            mass: s.hammer_mass,
            k: s.felt_k,
            p: s.felt_p,
            compliance: dt * dt / (2.0 * s.hammer_mass) + compliance.max(0.0),
            dt,
            t: 0,
            apart: 0,
            max_t: (MAX_CONTACT * sr) as u32,
            gap: (RELEASE_GAP * sr).max(2.0) as u32,
            contacts: false,
        };
    }

    /// Cuts the voice at once.
    pub fn kill(&mut self) {
        self.active = false;
        self.live = 0;
        self.level = 0.0;
        self.hammer.touching = false;
    }

    /// Moves the damper to `e` (0 lifted, 1 resting on the strings).
    pub fn set_damper(&mut self, e: f32) {
        if (e - self.damper).abs() < 1e-3 && !(e == 0.0 && self.damper != 0.0) {
            return;
        }
        self.damper = e;
        // A felt barely touching the string (half pedal) damps much less
        // than a resting one.
        let grip = e * e;
        for i in 0..self.live {
            // e^{−x} to the third order: x stays under 0.01.
            let x = grip * self.dmp[i];
            let m = 1.0 - x * (1.0 - x * (0.5 - x * (1.0 / 6.0)));
            self.cr[i] = self.br[i] * m;
            self.ci[i] = self.bi[i] * m;
        }
    }

    /// Renders `out.len()` (≤ [`BLOCK`]) samples into `out` (overwritten),
    /// before the voice's gain.
    pub fn render(&mut self, out: &mut [f32]) {
        let len = out.len().min(BLOCK);
        let mut start = 0;
        while start < len && self.hammer.touching {
            out[start] = self.contact_sample();
            start += 1;
        }
        if start < len {
            self.free(&mut out[start..len]);
        }
        self.cull();
    }

    /// One sample with the hammer on the string.
    // Lanes index several arrays at once.
    #[allow(clippy::needless_range_loop)]
    fn contact_sample(&mut self) -> f32 {
        let n = self.n;
        // Free motion over the sample.
        let mut y_free = [0.0f32; LANES];
        for g in (0..n).step_by(LANES) {
            for j in 0..LANES {
                let i = g + j;
                let (zr, zi) = (self.zr[i], self.zi[i]);
                let (cr, ci) = (self.cr[i], self.ci[i]);
                let r = zr * cr - zi * ci;
                let im = zr * ci + zi * cr;
                self.zr[i] = r;
                self.zi[i] = im;
                y_free[j] += self.hin[i] * im;
            }
        }
        let y_free: f32 = y_free.iter().sum();
        let h = &mut self.hammer;
        let dt = h.dt;
        // Compression without force, then the force that balances it.
        let free = h.y + h.v * dt - y_free;
        let force = if free > 0.0 { felt_force(free, h.k, h.p, h.compliance) } else { 0.0 };
        h.y += h.v * dt - force * dt * dt / (2.0 * h.mass);
        h.v -= force * dt / h.mass;
        h.t += 1;
        if force > 0.0 {
            h.contacts = true;
            h.apart = 0;
        } else {
            h.apart += 1;
        }
        let done = (h.contacts && h.apart > h.gap && h.v < 0.0) || h.t > h.max_t;
        let mut out = [0.0f32; LANES];
        for g in (0..n).step_by(LANES) {
            for j in 0..LANES {
                let i = g + j;
                self.zr[i] += self.inr[i] * force;
                self.zi[i] += self.ini[i] * force;
                out[j] += self.w[i] * self.zi[i];
            }
        }
        if done {
            self.hammer.touching = false;
            // From now on the phasors carry their radiated amplitude.
            for i in 0..n {
                self.zr[i] *= self.w[i];
                self.zi[i] *= self.w[i];
            }
            self.cull();
        }
        out.iter().sum()
    }

    /// Free vibration: eight phasors at a time, per-lane sums.
    fn free(&mut self, out: &mut [f32]) {
        let len = out.len();
        let mut acc = [[0.0f32; LANES]; BLOCK];
        for g in (0..self.live).step_by(LANES) {
            let mut zr: [f32; LANES] = self.zr[g..g + LANES].try_into().unwrap_or([0.0; LANES]);
            let mut zi: [f32; LANES] = self.zi[g..g + LANES].try_into().unwrap_or([0.0; LANES]);
            let cr: [f32; LANES] = self.cr[g..g + LANES].try_into().unwrap_or([0.0; LANES]);
            let ci: [f32; LANES] = self.ci[g..g + LANES].try_into().unwrap_or([0.0; LANES]);
            for a in acc.iter_mut().take(len) {
                for j in 0..LANES {
                    let r = zr[j] * cr[j] - zi[j] * ci[j];
                    let im = zr[j] * ci[j] + zi[j] * cr[j];
                    zr[j] = r;
                    zi[j] = im;
                    a[j] += im;
                }
            }
            self.zr[g..g + LANES].copy_from_slice(&zr);
            self.zi[g..g + LANES].copy_from_slice(&zi);
        }
        for (o, a) in out.iter_mut().zip(&acc) {
            *o = a.iter().sum();
        }
    }

    /// Drops the high partials that fell under audibility and measures the
    /// power left; the voice ends when nothing is.
    #[allow(clippy::needless_range_loop)]
    fn cull(&mut self) {
        if self.hammer.touching {
            self.level = 1.0;
            return;
        }
        // −110 dB at the output.
        let floor = (3e-6 / self.gain.max(1e-9)).powi(2);
        while self.live > 0 {
            let g = self.live - LANES;
            let loud = (g..self.live).any(|i| self.zr[i] * self.zr[i] + self.zi[i] * self.zi[i] > floor);
            if loud {
                break;
            }
            for i in g..self.live {
                self.zr[i] = 0.0;
                self.zi[i] = 0.0;
            }
            self.live = g;
        }
        let mut level = [0.0f32; LANES];
        for g in (0..self.live).step_by(LANES) {
            for j in 0..LANES {
                let i = g + j;
                level[j] += self.zr[i] * self.zr[i] + self.zi[i] * self.zi[i];
            }
        }
        self.level = 0.5 * level.iter().sum::<f32>();
        if self.live == 0 {
            self.active = false;
        }
    }

    /// Mean power of the radiated sound right after the strike, for the
    /// calibration of the keys.
    pub fn power(&self) -> f32 {
        0.5 * (0..self.n).map(|i| self.zr[i] * self.zr[i] + self.zi[i] * self.zi[i]).sum::<f32>()
    }
}

/// Felt force for a compression `free` reached without force: solves
/// x + C·K·x^p = free (x: actual compression) by Newton from above, where
/// the convex left side converges monotonically.
fn felt_force(free: f32, k: f32, p: f32, compliance: f32) -> f32 {
    let ck = compliance * k;
    // Both bounds lie above the root.
    let mut x = if ck > 0.0 { free.min((free / ck).powf(1.0 / p)) } else { free };
    for _ in 0..12 {
        let xp1 = x.powf(p - 1.0);
        let h = x + ck * xp1 * x - free;
        let dh = 1.0 + ck * p * xp1;
        let next = (x - h / dh).max(0.0);
        if (x - next).abs() <= 1e-7 * free {
            x = next;
            break;
        }
        x = next;
    }
    k * x.powf(p)
}

/// Lays out the modes of a strike: `push` returns false when full.
fn layout(s: &Setup, max_hz: f32, mut push: impl FnMut(Mode) -> bool) {
    let after = 6.9 / s.t60.max(0.05);
    let prompt = after / s.prompt_ratio.clamp(0.05, 1.0);
    let damped = 6.9 / s.damped_t60.max(0.01);
    let cents = |c: f32| (c * (1.0 / 1200.0)).exp2();
    match s.material {
        Material::Strings => {
            let slow = if s.strings <= 1 { 1 } else { s.strings - 1 };
            let slow_gain = s.asym / (slow as f32).sqrt();
            for n in 1..=MAX_MODES {
                let nf = n as f32;
                let hz = s.f1 * super::keys::partial_ratio(nf, s.b);
                if hz > max_hz {
                    break;
                }
                // Losses growing as f² above the fundamental, whose decay
                // is the key's own.
                let (rel, rel1) = (hz * (1.0 / 2000.0), s.f1 * (1.0 / 2000.0));
                let hf = s.hf_loss * (rel * rel - rel1 * rel1);
                let shape = (PI * nf * s.strike).sin();
                let sign = if n % 2 == 1 { 1.0 } else { -1.0 };
                // Dampers grab the upper partials harder.
                let damper_sigma = damped * (1.0 + 0.25 * (nf - 1.0).sqrt());
                let sym = Mode { hz, sigma: prompt + hf, damper_sigma, shape: shape * s.sym, weight: sign * nf };
                if !push(sym) {
                    return;
                }
                if n <= SLOW_PARTIALS {
                    for k in 0..slow {
                        let m = Mode {
                            hz: hz * cents(s.detune[k]),
                            sigma: after + 0.9 * hf,
                            damper_sigma,
                            shape: shape * slow_gain,
                            weight: sign * nf * s.slow_out,
                        };
                        if !push(m) {
                            return;
                        }
                    }
                }
            }
        }
        Material::Rods | Material::Bars => {
            let rods = s.material == Material::Rods;
            let ratios = if rods { &ROD_RATIOS } else { &BAR_RATIOS };
            for (n, &ratio) in ratios.iter().enumerate() {
                let hz = s.f1 * ratio;
                if hz > max_hz {
                    break;
                }
                // Overtones of metal die much faster than the fundamental.
                let sigma = after * ratio.powf(if rods { 0.9 } else { 1.3 });
                let shape = if rods { 1.0 / (1.0 + 0.8 * n as f32) } else { [1.0, 0.45, 0.6, 0.3, 0.25, 0.2][n] };
                // The celesta's resonators reinforce the fundamental.
                let weight = if n == 0 && !rods { 2.5 } else { 1.0 };
                let modes = [(1.0, s.sym, 1.0), (cents(s.detune[0]), s.asym, s.slow_out)];
                for (k, &(det, gain, out)) in modes.iter().enumerate() {
                    let m = Mode {
                        hz: hz * det,
                        sigma: if k == 0 { sigma } else { sigma * 0.8 },
                        damper_sigma: damped * (1.0 + n as f32),
                        shape: shape * gain,
                        weight: weight * out,
                    };
                    if !push(m) {
                        return;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn felt_force_balances_the_compression() {
        for &(free, k, p, c) in &[(1e-3f32, 4e9f32, 2.5f32, 1e-9f32), (5e-4, 1e12, 3.0, 2e-9), (2e-3, 4e8, 2.3, 1e-8)] {
            let f = felt_force(free, k, p, c);
            let x = (f / k).powf(1.0 / p);
            assert!(f > 0.0 && f.is_finite());
            assert!((x + c * f - free).abs() < 1e-3 * free, "{free} {f}");
        }
    }
}
