/// Convertit une note MIDI (69 = La 440 Hz) en fréquence.
#[inline]
pub fn midi_to_hz(note: f32) -> f32 {
    440.0 * 2f32.powf((note - 69.0) / 12.0)
}

#[inline]
pub fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

#[inline]
pub fn gain_to_db(gain: f32) -> f32 {
    20.0 * gain.max(1e-10).log10()
}

/// Balance stéréo : `pan` dans [-1, 1], gain unitaire au centre.
#[inline]
pub fn balance_gains(pan: f32) -> (f32, f32) {
    let pan = pan.clamp(-1.0, 1.0);
    ((1.0 - pan).min(1.0), (1.0 + pan).min(1.0))
}

/// Coefficient multiplicatif pour une décroissance exponentielle
/// de constante de temps `tau` secondes.
#[inline]
pub fn decay_coef(tau: f32, sample_rate: f32) -> f32 {
    (-1.0 / (tau.max(1e-6) * sample_rate)).exp()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn midi_reference_pitches() {
        assert!((midi_to_hz(69.0) - 440.0).abs() < 1e-3);
        assert!((midi_to_hz(57.0) - 220.0).abs() < 1e-3);
        assert!((midi_to_hz(60.0) - 261.6256).abs() < 1e-2);
    }

    #[test]
    fn db_roundtrip() {
        for db in [-60.0, -6.0, 0.0, 6.0] {
            assert!((gain_to_db(db_to_gain(db)) - db).abs() < 1e-3);
        }
    }

    #[test]
    fn balance_is_unity_at_center() {
        assert_eq!(balance_gains(0.0), (1.0, 1.0));
        assert_eq!(balance_gains(-1.0), (1.0, 0.0));
        assert_eq!(balance_gains(1.0), (0.0, 1.0));
    }
}
