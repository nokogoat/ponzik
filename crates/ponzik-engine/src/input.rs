//! Entrée audio et enregistrement : le thread de capture pousse des trames
//! stéréo dans une file SPSC, le moteur les lit à chaque bloc.

use crate::ring::{self, Consumer, Producer};
use crate::sample::Sample;

/// Côté capture : à appeler depuis la fonction de rappel du pilote d'entrée.
pub struct InputProducer {
    ring: Producer<[f32; 2]>,
}

/// Côté moteur : confié au moteur par [`crate::Command::SetInput`].
pub struct InputConsumer {
    ring: Consumer<[f32; 2]>,
}

/// File d'entrée d'au moins `capacity` trames stéréo (≈ 2 à 4 fois la période
/// du pilote suffit ; le retard est borné à la moitié de la file).
pub fn input_channel(capacity: usize) -> (InputProducer, InputConsumer) {
    let (tx, rx) = ring::channel(capacity);
    (InputProducer { ring: tx }, InputConsumer { ring: rx })
}

impl InputProducer {
    /// Pousse des trames entrelacées de `channels` canaux : le mono est dupliqué,
    /// au-delà de deux canaux seuls les deux premiers sont gardés. Renvoie le
    /// nombre de trames acceptées (le reste est perdu si le moteur ne suit pas).
    pub fn push(&mut self, data: &[f32], channels: usize) -> usize {
        if channels == 0 {
            return 0;
        }
        let mut pushed = 0;
        for frame in data.chunks_exact(channels) {
            let stereo = match frame {
                [m] => [*m; 2],
                [l, r, ..] => [*l, *r],
                [] => break,
            };
            if self.ring.push(stereo).is_err() {
                break;
            }
            pushed += 1;
        }
        pushed
    }
}

impl InputConsumer {
    /// Remplit `left`/`right` (même longueur) ; complète en silence si la capture
    /// est en retard. Renvoie le nombre de trames réellement reçues. Sans
    /// allocation ni verrou.
    pub fn read(&mut self, left: &mut [f32], right: &mut [f32]) -> usize {
        let n = left.len().min(right.len());
        // Horloges de capture et de sortie légèrement différentes : au-delà d'une
        // demi-file d'avance, on jette le plus ancien pour borner le retard.
        let excess = self.ring.len().saturating_sub(n + self.ring.capacity() / 2);
        for _ in 0..excess {
            self.ring.pop();
        }
        let mut got = 0;
        while got < n {
            let Some([l, r]) = self.ring.pop() else { break };
            left[got] = l;
            right[got] = r;
            got += 1;
        }
        left[got..n].fill(0.0);
        right[got..n].fill(0.0);
        got
    }
}

/// Tampon d'enregistrement préalloué par l'interface, rempli par le moteur
/// puis rendu par [`crate::EngineHandle::take_recordings`].
///
/// La capacité est la longueur de `left`/`right` ; `len` trames sont écrites.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RecordBuffer {
    pub left: Vec<f32>,
    pub right: Vec<f32>,
    /// Trames écrites.
    pub len: usize,
    /// Position du transport (ticks) à la première trame écrite.
    pub start_tick: f64,
    /// Fréquence du moteur, fixée à l'armement.
    pub sample_rate: f32,
    /// Piste de mixer armée, fixée à l'armement.
    pub track: usize,
}

impl RecordBuffer {
    /// Alloue `frames` trames. La mémoire est écrite tout de suite (pas de
    /// `calloc` paresseux) : aucun défaut de page sur le thread audio.
    pub fn new(frames: usize, sample_rate: f32) -> Self {
        let zeroed = || {
            let mut v = Vec::with_capacity(frames);
            v.resize(frames, 0.0);
            v
        };
        Self { left: zeroed(), right: zeroed(), len: 0, start_tick: 0.0, sample_rate, track: 0 }
    }

    /// Tampon de `seconds` secondes (par exemple 600 pour 10 minutes).
    pub fn with_duration(seconds: f32, sample_rate: f32) -> Self {
        Self::new((seconds.max(0.0) * sample_rate) as usize, sample_rate)
    }

    pub fn capacity(&self) -> usize {
        self.left.len().min(self.right.len())
    }

    pub fn is_full(&self) -> bool {
        self.len >= self.capacity()
    }

    /// Ajoute ce qui tient ; renvoie le nombre de trames écrites.
    pub fn write(&mut self, left: &[f32], right: &[f32]) -> usize {
        let n = left.len().min(right.len()).min(self.capacity().saturating_sub(self.len));
        self.left[self.len..self.len + n].copy_from_slice(&left[..n]);
        self.right[self.len..self.len + n].copy_from_slice(&right[..n]);
        self.len += n;
        n
    }

    /// Échantillon des trames écrites (le surplus préalloué est libéré).
    pub fn into_sample(mut self, name: impl Into<String>) -> Sample {
        self.left.truncate(self.len);
        self.right.truncate(self.len);
        self.left.shrink_to_fit();
        self.right.shrink_to_fit();
        Sample { name: name.into(), sample_rate: self.sample_rate, left: self.left, right: self.right }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mono_is_duplicated_and_extra_channels_dropped() {
        let (mut tx, mut rx) = input_channel(16);
        assert_eq!(tx.push(&[0.1, 0.2], 1), 2);
        assert_eq!(tx.push(&[0.3, 0.4, 9.0, 0.5, 0.6, 9.0], 3), 2);
        let (mut l, mut r) = ([0.0; 6], [0.0; 6]);
        assert_eq!(rx.read(&mut l, &mut r), 4);
        assert_eq!(l, [0.1, 0.2, 0.3, 0.5, 0.0, 0.0]);
        assert_eq!(r, [0.1, 0.2, 0.4, 0.6, 0.0, 0.0]);
    }

    #[test]
    fn full_queue_drops_and_latency_is_bounded() {
        let (mut tx, mut rx) = input_channel(64);
        let data: Vec<f32> = (0..200).map(|i| i as f32).collect();
        assert_eq!(tx.push(&data, 1), 64);
        // 64 en attente, on en lit 8 : on garde au plus 8 + 32, les plus récents.
        let (mut l, mut r) = ([0.0; 8], [0.0; 8]);
        assert_eq!(rx.read(&mut l, &mut r), 8);
        assert_eq!(l[0], 24.0);
        assert_eq!(rx.ring.len(), 32);
    }

    #[test]
    fn record_buffer_stops_when_full() {
        let mut b = RecordBuffer::new(5, 48_000.0);
        assert_eq!(b.write(&[1.0; 3], &[2.0; 3]), 3);
        assert_eq!(b.write(&[1.0; 3], &[2.0; 3]), 2);
        assert!(b.is_full());
        assert_eq!(b.write(&[1.0; 3], &[2.0; 3]), 0);
        let s = b.into_sample("prise");
        assert_eq!((s.len(), s.right[4]), (5, 2.0));
    }
}
