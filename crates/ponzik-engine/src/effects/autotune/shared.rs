//! What the pitch corrector shows the interface, written on the audio thread
//! without locks: a ring of pitch points (one u64 each, no tearing) and the
//! state of the moment.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use crate::effect::ChannelNotes;

/// Points kept: 8 s at one point every 4 ms.
pub const HISTORY: usize = 2_048;

/// One point of the pitch graph.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PitchPoint {
    /// Pitch of the voice and pitch sent out (MIDI, fractional; NaN when unvoiced).
    pub input: f32,
    pub output: f32,
    /// Note aimed at.
    pub note: Option<u8>,
    /// The note is left alone (bypass, nothing to aim at).
    pub bypassed: bool,
    pub level_db: f32,
}

const NONE16: u64 = 0;

fn pack_pitch(p: f32) -> u64 {
    if p.is_finite() { ((p * 100.0).round().clamp(0.0, 13_000.0) as u64) + 1 } else { NONE16 }
}

fn unpack_pitch(v: u64) -> f32 {
    if v == NONE16 { f32::NAN } else { (v - 1) as f32 / 100.0 }
}

impl PitchPoint {
    pub fn pack(&self) -> u64 {
        let note = self.note.map_or(255, u64::from);
        let level = (self.level_db + 120.0).round().clamp(0.0, 254.0) as u64;
        pack_pitch(self.input) | pack_pitch(self.output) << 16 | note << 32 | u64::from(self.bypassed) << 40 | level << 48
    }

    pub fn unpack(v: u64) -> Self {
        let note = (v >> 32 & 0xFF) as u8;
        Self {
            input: unpack_pitch(v & 0xFFFF),
            output: unpack_pitch(v >> 16 & 0xFFFF),
            note: (note != 255).then_some(note),
            bypassed: v >> 40 & 1 == 1,
            level_db: (v >> 48 & 0xFF) as f32 - 120.0,
        }
    }
}

pub struct AutoTuneShared {
    points: Vec<AtomicU64>,
    written: AtomicU64,
    /// Seconds between two points (f32 bits).
    point_secs: AtomicU32,
    latency: AtomicU32,
    /// Notes held on the MIDI source channel.
    midi: [AtomicU64; 2],
}

impl AutoTuneShared {
    pub fn new(point_secs: f32) -> Self {
        Self {
            points: (0..HISTORY).map(|_| AtomicU64::new(PitchPoint::default().pack())).collect(),
            written: AtomicU64::new(0),
            point_secs: AtomicU32::new(point_secs.to_bits()),
            latency: AtomicU32::new(0),
            midi: [AtomicU64::new(0), AtomicU64::new(0)],
        }
    }

    pub fn push(&self, p: PitchPoint) {
        let n = self.written.load(Ordering::Relaxed);
        self.points[n as usize % HISTORY].store(p.pack(), Ordering::Relaxed);
        self.written.store(n + 1, Ordering::Release);
    }

    /// Points written so far.
    pub fn written(&self) -> u64 {
        self.written.load(Ordering::Acquire)
    }

    pub fn point_secs(&self) -> f32 {
        f32::from_bits(self.point_secs.load(Ordering::Relaxed))
    }

    pub(crate) fn set_point_secs(&self, s: f32) {
        self.point_secs.store(s.to_bits(), Ordering::Relaxed);
    }

    /// The last `out.len()` points (or fewer), oldest first; returns how many.
    pub fn recent(&self, out: &mut [PitchPoint]) -> usize {
        let n = self.written();
        let k = (out.len() as u64).min(n).min(HISTORY as u64 - 64) as usize;
        let first = n - k as u64;
        for (i, dst) in out[..k].iter_mut().enumerate() {
            *dst = PitchPoint::unpack(self.points[(first as usize + i) % HISTORY].load(Ordering::Relaxed));
        }
        k
    }

    /// Latency of the effect, in samples.
    pub fn latency(&self) -> u32 {
        self.latency.load(Ordering::Relaxed)
    }

    pub(crate) fn set_latency(&self, samples: usize) {
        self.latency.store(samples as u32, Ordering::Relaxed);
    }

    pub(crate) fn set_midi(&self, n: &ChannelNotes) {
        self.midi[0].store(n.held[0], Ordering::Relaxed);
        self.midi[1].store(n.held[1], Ordering::Relaxed);
    }

    /// Notes held on the MIDI source channel.
    pub fn midi(&self) -> ChannelNotes {
        ChannelNotes { held: [self.midi[0].load(Ordering::Relaxed), self.midi[1].load(Ordering::Relaxed)], last: None }
    }
}
