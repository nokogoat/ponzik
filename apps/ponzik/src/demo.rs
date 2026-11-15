//! Projet de démo : un groove house en La mineur, 100 % instruments natifs.

use ponzik_engine::Channel;
use ponzik_engine::instruments::{DrumKind, DrumSynth, Pulse, PulseParams};

pub const BPM: f32 = 124.0;

pub fn rack() -> Vec<Channel> {
    let drum = |kind| Box::new(DrumSynth::new(kind));
    vec![
        Channel::new("Kick", drum(DrumKind::Kick)).with_hits(&[0, 4, 8, 12], 1.0),
        Channel::new("Clap", drum(DrumKind::Clap)).with_hits(&[4, 12], 0.8).with_gain(0.8),
        Channel::new("Hat", drum(DrumKind::ClosedHat))
            .with_hits(&[1, 3, 5, 7, 9, 11, 13, 15], 0.45)
            .with_pan(-0.3),
        Channel::new("Open Hat", drum(DrumKind::OpenHat))
            .with_hits(&[2, 6, 10, 14], 0.6)
            .with_gain(0.7)
            .with_pan(0.3),
        Channel::new("Bass", Box::new(Pulse::new(PulseParams::default())))
            .with_root_note(45)
            .with_notes(
                &[(0, 33, 0.8), (3, 45, 0.5), (6, 33, 0.8), (8, 36, 0.8), (10, 43, 0.5), (11, 45, 0.5), (14, 40, 1.5)],
                0.9,
            ),
    ]
}
