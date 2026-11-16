//! Accordeur : détection de hauteur par l'algorithme YIN (de Cheveigné et Kawahara, 2002), avec
//! interpolation parabolique du minimum, puis conversion en note et en cents.
//!
//! Tampons alloués à la construction ; `push` et `detect` n'allouent rien.

/// Noms français des douze notes, en partant de Do.
pub const NOTE_NAMES: [&str; 12] = ["Do", "Do#", "Ré", "Ré#", "Mi", "Fa", "Fa#", "Sol", "Sol#", "La", "La#", "Si"];

/// Noms anglais des douze notes, en partant de C.
pub const NOTE_LETTERS: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

/// Hauteur détectée.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pitch {
    /// Fréquence fondamentale en Hz.
    pub hz: f32,
    /// Note MIDI la plus proche (69 = La 4).
    pub note: i32,
    /// Écart à cette note, entre −50 et +50 cents.
    pub cents: f32,
    /// Netteté de la périodicité, de 0 (bruit) à 1 (sinus pur).
    pub clarity: f32,
}

impl Pitch {
    /// Nom français de la note (« La »).
    pub fn name(&self) -> &'static str {
        NOTE_NAMES[self.note.rem_euclid(12) as usize]
    }

    /// Nom anglais de la note (« A »).
    pub fn letter(&self) -> &'static str {
        NOTE_LETTERS[self.note.rem_euclid(12) as usize]
    }

    /// Octave en notation scientifique (Do 4 = MIDI 60).
    pub fn octave(&self) -> i32 {
        self.note.div_euclid(12) - 1
    }
}

/// Détecteur de hauteur alimenté en continu ; `detect` analyse les `window` derniers échantillons.
#[derive(Clone, Debug)]
pub struct Tuner {
    sample_rate: f32,
    ring: Vec<f32>,
    pos: usize,
    filled: usize,
    frame: Vec<f32>,
    /// Fonction de différence, puis différence normalisée cumulée.
    diff: Vec<f32>,
    cmnd: Vec<f32>,
    threshold: f32,
    reference_hz: f32,
    min_hz: f32,
    max_hz: f32,
}

impl Tuner {
    /// `window` : taille d'analyse (au moins 64) ; la plus basse fréquence détectable vaut
    /// `sample_rate / (window / 2)` (2048 à 48 kHz : environ 47 Hz).
    pub fn new(sample_rate: f32, window: usize) -> Self {
        assert!(window >= 64, "fenêtre d'accordeur trop courte : {window}");
        let half = window / 2;
        Self {
            sample_rate,
            ring: vec![0.0; window],
            pos: 0,
            filled: 0,
            frame: vec![0.0; window],
            diff: vec![0.0; half + 1],
            cmnd: vec![0.0; half + 1],
            threshold: 0.15,
            reference_hz: 440.0,
            min_hz: 20.0,
            max_hz: 5000.0,
        }
    }

    /// Fréquence du La 4 (440 Hz par défaut).
    pub fn set_reference(&mut self, hz: f32) {
        self.reference_hz = hz.max(1.0);
    }

    /// Plage de recherche en Hz (bornée en bas par la taille de la fenêtre).
    pub fn set_range(&mut self, min_hz: f32, max_hz: f32) {
        self.min_hz = min_hz.max(1.0);
        self.max_hz = max_hz.max(self.min_hz);
    }

    /// Seuil YIN sur la différence normalisée (0,1 à 0,2 ; plus bas = plus exigeant).
    pub fn set_threshold(&mut self, threshold: f32) {
        self.threshold = threshold.clamp(0.01, 1.0);
    }

    pub fn reset(&mut self) {
        self.ring.fill(0.0);
        (self.pos, self.filled) = (0, 0);
    }

    pub fn push(&mut self, samples: &[f32]) {
        let n = self.ring.len();
        for &x in samples {
            self.ring[self.pos] = x;
            self.pos = (self.pos + 1) % n;
        }
        self.filled = (self.filled + samples.len()).min(n);
    }

    /// Hauteur des derniers échantillons, ou `None` (fenêtre incomplète, silence, signal non périodique).
    pub fn detect(&mut self) -> Option<Pitch> {
        let n = self.ring.len();
        if self.filled < n {
            return None;
        }
        let (newer, older) = self.ring.split_at(self.pos);
        for (dst, &x) in self.frame.iter_mut().zip(older.iter().chain(newer)) {
            *dst = x;
        }
        let energy = self.frame.iter().map(|x| x * x).sum::<f32>() / n as f32;
        if energy < 1e-8 {
            return None;
        }

        let half = n / 2;
        let tau_min = ((self.sample_rate / self.max_hz).floor() as usize).max(2);
        let tau_max = ((self.sample_rate / self.min_hz).ceil() as usize).min(half - 1);
        if tau_min + 1 >= tau_max {
            return None;
        }
        let x = &self.frame;
        for tau in 0..=tau_max + 1 {
            self.diff[tau] = (0..half).map(|j| x[j] - x[j + tau]).map(|d| d * d).sum();
        }
        self.cmnd[0] = 1.0;
        let mut running = 0.0;
        for tau in 1..=tau_max + 1 {
            running += self.diff[tau];
            self.cmnd[tau] = if running > 0.0 { self.diff[tau] * tau as f32 / running } else { 1.0 };
        }

        // Premier creux sous le seuil, puis descente jusqu'au minimum local.
        let mut tau = (tau_min..=tau_max).find(|&t| self.cmnd[t] < self.threshold)?;
        while tau < tau_max && self.cmnd[tau + 1] < self.cmnd[tau] {
            tau += 1;
        }
        let clarity = (1.0 - self.cmnd[tau]).clamp(0.0, 1.0);

        // Parabole sur la fonction de différence brute autour du minimum.
        let (a, b, c) = (self.diff[tau - 1], self.diff[tau], self.diff[tau + 1]);
        let den = a - 2.0 * b + c;
        let shift = if den > 0.0 { (0.5 * (a - c) / den).clamp(-1.0, 1.0) } else { 0.0 };
        let hz = self.sample_rate / (tau as f32 + shift);

        let midi = 69.0 + 12.0 * (hz / self.reference_hz).log2();
        let note = midi.round();
        Some(Pitch { hz, note: note as i32, cents: (midi - note) * 100.0, clarity })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::noise::WhiteNoise;
    use std::f64::consts::TAU;

    fn sine(hz: f64, sr: f64, n: usize) -> Vec<f32> {
        (0..n).map(|i| (0.5 * (TAU * hz * i as f64 / sr).sin()) as f32).collect()
    }

    fn cents(a: f32, b: f32) -> f32 {
        1200.0 * (a / b).log2()
    }

    #[test]
    fn sines_within_two_cents() {
        for sr in [44_100.0, 48_000.0] {
            let mut t = Tuner::new(sr as f32, 2048);
            for hz in [55.0, 82.407, 110.0, 196.0, 261.626, 440.0, 466.164, 659.255, 1046.502, 1975.533, 3520.0] {
                t.reset();
                t.push(&sine(hz, sr, 4096));
                let p = t.detect().unwrap_or_else(|| panic!("{hz} Hz non détecté"));
                let err = cents(p.hz, hz as f32);
                assert!(err.abs() < 2.0, "{hz} Hz à {sr} : lu {} Hz ({err} cents)", p.hz);
                assert!(p.cents.abs() < 2.0 && p.clarity > 0.9, "{hz} Hz : {p:?}");
            }
        }
    }

    #[test]
    fn note_names_and_cents() {
        let mut t = Tuner::new(48_000.0, 2048);
        let detuned = 440.0 * 2f64.powf(13.0 / 1200.0);
        t.push(&sine(detuned, 48_000.0, 2048));
        let p = t.detect().unwrap();
        assert_eq!((p.note, p.name(), p.letter(), p.octave()), (69, "La", "A", 4));
        assert!((p.cents - 13.0).abs() < 2.0, "{p:?}");

        t.push(&sine(261.626 * 2f64.powf(-30.0 / 1200.0), 48_000.0, 2048));
        let p = t.detect().unwrap();
        assert_eq!((p.note, p.name(), p.octave()), (60, "Do", 4));
        assert!((p.cents + 30.0).abs() < 2.0, "{p:?}");

        t.set_reference(442.0);
        t.push(&sine(442.0, 48_000.0, 2048));
        let p = t.detect().unwrap();
        assert!(p.note == 69 && p.cents.abs() < 2.0, "{p:?}");
    }

    #[test]
    fn nothing_on_silence_noise_or_partial_window() {
        let mut t = Tuner::new(48_000.0, 2048);
        t.push(&sine(440.0, 48_000.0, 1000));
        assert_eq!(t.detect(), None);
        t.push(&vec![0.0; 2048]);
        assert_eq!(t.detect(), None);
        let mut noise = WhiteNoise::new(5);
        let block: Vec<f32> = (0..2048).map(|_| noise.next_sample()).collect();
        t.push(&block);
        assert_eq!(t.detect(), None);
    }

    #[test]
    fn rich_harmonics_keep_the_fundamental() {
        // Dent de scie à 10 harmoniques : YIN ne doit pas sauter à l'octave.
        let (sr, hz) = (48_000.0, 147.0);
        let x: Vec<f32> = (0..2048)
            .map(|i| (1..=10).map(|k| (TAU * hz * k as f64 * i as f64 / sr).sin() / k as f64).sum::<f64>() as f32 * 0.3)
            .collect();
        let mut t = Tuner::new(sr as f32, 2048);
        t.push(&x);
        let p = t.detect().unwrap();
        assert!(cents(p.hz, hz as f32).abs() < 2.0, "{p:?}");
    }
}
