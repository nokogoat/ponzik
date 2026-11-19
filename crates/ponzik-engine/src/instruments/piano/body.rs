//! The case: soundboard, lid and the room around the piano.
//!
//! The bridge force reaches the listener through the soundboard, a plate whose
//! low modes (from about 50 Hz, dense above a few hundred) color the tone and
//! ring briefly after a blow: the "wood". Each mode radiates with its own
//! sign and weight to each ear, which widens the image. The lid, open or
//! closed, sets the brilliance; a small feedback delay network adds the
//! early reverberation of the room.

use ponzik_dsp::biquad::{Biquad, BiquadCoefs};

/// Soundboard modes of a concert grand: (Hz, Q, gain).
const BOARD: [(f32, f32, f32); 20] = [
    (49.0, 9.0, 0.7),
    (68.0, 11.0, 0.9),
    (92.0, 12.0, 1.0),
    (117.0, 14.0, 0.95),
    (146.0, 15.0, 1.0),
    (178.0, 16.0, 0.9),
    (213.0, 18.0, 0.85),
    (258.0, 18.0, 0.8),
    (304.0, 20.0, 0.75),
    (366.0, 20.0, 0.7),
    (437.0, 22.0, 0.62),
    (515.0, 22.0, 0.55),
    (608.0, 24.0, 0.5),
    (721.0, 24.0, 0.45),
    (853.0, 25.0, 0.4),
    (1_004.0, 25.0, 0.35),
    (1_210.0, 26.0, 0.3),
    (1_460.0, 26.0, 0.26),
    (1_770.0, 28.0, 0.22),
    (2_130.0, 28.0, 0.18),
];

/// Delay lengths of the room, ms.
const ROOM_MS: [f32; 4] = [17.3, 23.9, 29.1, 37.7];
/// Reverberation time of the room, s.
const ROOM_T60: f32 = 0.9;

#[derive(Clone, Copy, Debug, Default)]
pub struct BodySettings {
    /// Soundboard color mixed in, 0 to 1.
    pub board: f32,
    /// Lid opening, 0 (closed) to 1 (full stick).
    pub lid: f32,
    /// Felt moderator, 0 to 1: muffles the treble.
    pub felt: f32,
    /// 0 (upright) to 1 (concert grand).
    pub size: f32,
    pub room: f32,
}

pub struct Body {
    sample_rate: f32,
    board: [Biquad; BOARD.len()],
    /// Radiation of each mode to each ear.
    board_l: [f32; BOARD.len()],
    board_r: [f32; BOARD.len()],
    board_mix: f32,
    /// Direct path: low cut, lid shelf and lowpass, per ear.
    low_cut: [Biquad; 2],
    shelf: [Biquad; 2],
    lowpass: [Biquad; 2],
    room: Vec<f32>,
    room_start: [usize; 4],
    room_len: [usize; 4],
    room_pos: [usize; 4],
    room_lp: [f32; 4],
    room_g: [f32; 4],
    room_damp: f32,
    room_mix: f32,
}

impl Body {
    pub fn new(sample_rate: f32) -> Self {
        let mut start = [0; 4];
        let mut len = [0; 4];
        let mut total = 0;
        for i in 0..4 {
            start[i] = total;
            len[i] = (ROOM_MS[i] * 1e-3 * sample_rate).round().max(1.0) as usize;
            total += len[i];
        }
        let mut board_l = [0.0; BOARD.len()];
        let mut board_r = [0.0; BOARD.len()];
        for i in 0..BOARD.len() {
            // Fixed pseudo-random radiation pattern: same modes, other mixes.
            let a = super::keys::hash(i as u8, 11);
            let b = super::keys::hash(i as u8, 23);
            board_l[i] = BOARD[i].2 * (0.6 + 0.4 * a).copysign(a + 0.3);
            board_r[i] = BOARD[i].2 * (0.6 + 0.4 * b).copysign(b + 0.3);
        }
        let mut body = Self {
            sample_rate,
            board: std::array::from_fn(|_| Biquad::default()),
            board_l,
            board_r,
            board_mix: 0.0,
            low_cut: Default::default(),
            shelf: Default::default(),
            lowpass: Default::default(),
            room: vec![0.0; total],
            room_start: start,
            room_len: len,
            room_pos: [0; 4],
            room_lp: [0.0; 4],
            room_g: [0.0; 4],
            room_damp: 0.3,
            room_mix: 0.0,
        };
        body.set(&BodySettings { board: 0.5, lid: 0.8, felt: 0.0, size: 1.0, room: 0.2 });
        body
    }

    pub fn clear(&mut self) {
        self.room.fill(0.0);
        self.room_lp = [0.0; 4];
        for f in self.board.iter_mut().chain(&mut self.low_cut).chain(&mut self.shelf).chain(&mut self.lowpass) {
            f.reset();
        }
    }

    /// New settings; filter states carry over (no allocation).
    pub fn set(&mut self, s: &BodySettings) {
        let sr = self.sample_rate;
        // A smaller board has higher, sparser modes.
        let scale = 1.0 + 0.35 * (1.0 - s.size.clamp(0.0, 1.0));
        for (f, &(hz, q, _)) in self.board.iter_mut().zip(&BOARD) {
            f.coefs = BiquadCoefs::bandpass(hz * scale, q, sr);
        }
        self.board_mix = s.board.clamp(0.0, 1.0) * 1.6;
        let lid = s.lid.clamp(0.0, 1.0);
        let felt = s.felt.clamp(0.0, 1.0);
        let shelf = BiquadCoefs::high_shelf(2_500.0, 0.7, -10.0 + 11.5 * lid - 10.0 * felt, sr);
        let cutoff = 4_500.0 * (4.0f32).powf(lid) * (0.25f32).powf(felt);
        let lowpass = BiquadCoefs::lowpass(cutoff.min(sr * 0.45), 0.6, sr);
        let low_cut = BiquadCoefs::highpass(if s.size < 0.5 { 45.0 } else { 30.0 }, 0.7, sr);
        for i in 0..2 {
            self.shelf[i].coefs = shelf;
            self.lowpass[i].coefs = lowpass;
            self.low_cut[i].coefs = low_cut;
        }
        for i in 0..4 {
            self.room_g[i] = 10f32.powf(-3.0 * self.room_len[i] as f32 / (ROOM_T60 * sr));
        }
        self.room_damp = (1.0 - (-std::f32::consts::TAU * 6_000.0 / sr).exp()).clamp(0.05, 1.0);
        self.room_mix = s.room.clamp(0.0, 1.0) * 0.5;
    }

    /// Processes the bridge signal of each ear in place.
    pub fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let mono = 0.5 * (*l + *r);
            let (mut bl, mut br) = (0.0, 0.0);
            for i in 0..BOARD.len() {
                let y = self.board[i].process(mono);
                bl += y * self.board_l[i];
                br += y * self.board_r[i];
            }
            let mut dl = *l + bl * self.board_mix;
            let mut dr = *r + br * self.board_mix;
            dl = self.lowpass[0].process(self.shelf[0].process(self.low_cut[0].process(dl)));
            dr = self.lowpass[1].process(self.shelf[1].process(self.low_cut[1].process(dr)));
            if self.room_mix > 0.0 {
                let (wl, wr) = self.room_sample(dl, dr);
                dl += wl * self.room_mix;
                dr += wr * self.room_mix;
            }
            *l = dl;
            *r = dr;
        }
    }

    /// One sample of the room: four lines, Hadamard feedback, damped.
    fn room_sample(&mut self, l: f32, r: f32) -> (f32, f32) {
        let mut out = [0.0f32; 4];
        for (i, o) in out.iter_mut().enumerate() {
            *o = self.room[self.room_start[i] + self.room_pos[i]];
        }
        let h = [
            out[0] + out[1] + out[2] + out[3],
            out[0] - out[1] + out[2] - out[3],
            out[0] + out[1] - out[2] - out[3],
            out[0] - out[1] - out[2] + out[3],
        ];
        let input = [l, r, l, r];
        for i in 0..4 {
            self.room_lp[i] += self.room_damp * (h[i] * 0.5 - self.room_lp[i]);
            self.room_lp[i] = if self.room_lp[i].abs() < 1e-20 { 0.0 } else { self.room_lp[i] };
            let v = self.room_lp[i] * self.room_g[i] + input[i] * 0.35;
            self.room[self.room_start[i] + self.room_pos[i]] = v;
            self.room_pos[i] += 1;
            if self.room_pos[i] >= self.room_len[i] {
                self.room_pos[i] = 0;
            }
        }
        (out[0] - 0.6 * out[2], out[1] - 0.6 * out[3])
    }
}
