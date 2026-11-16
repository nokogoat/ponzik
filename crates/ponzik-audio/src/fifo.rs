//! Tampons de l'entrée en duplex intégral : une file SPSC sans verrou entre un
//! thread de capture et le thread de sortie, et la file d'attente (« jitter
//! buffer ») du thread de sortie, qui rend à chaque cycle exactement le nombre
//! de trames demandé avec le moins de retard possible.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

#[cfg_attr(not(windows), allow(dead_code))]
struct Shared {
    /// Échantillons `f32` stockés en bits : aucun `unsafe` nécessaire.
    slots: Box<[AtomicU32]>,
    mask: usize,
    head: AtomicUsize,
    tail: AtomicUsize,
}

/// Côté capture d'une file d'échantillons.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) struct RingProducer {
    shared: Arc<Shared>,
}

/// Côté sortie d'une file d'échantillons.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) struct RingConsumer {
    shared: Arc<Shared>,
}

/// File d'au moins `capacity` échantillons (puissance de 2 supérieure).
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn ring(capacity: usize) -> (RingProducer, RingConsumer) {
    let capacity = capacity.max(2).next_power_of_two();
    let shared = Arc::new(Shared {
        slots: (0..capacity).map(|_| AtomicU32::new(0)).collect(),
        mask: capacity - 1,
        head: AtomicUsize::new(0),
        tail: AtomicUsize::new(0),
    });
    (RingProducer { shared: shared.clone() }, RingConsumer { shared })
}

#[cfg_attr(not(windows), allow(dead_code))]
impl RingProducer {
    /// Pousse ce qui tient, par trames entières de `channels` échantillons ;
    /// renvoie le nombre d'échantillons acceptés.
    pub(crate) fn push(&mut self, data: &[f32], channels: usize) -> usize {
        let s = &*self.shared;
        let tail = s.tail.load(Ordering::Relaxed);
        let head = s.head.load(Ordering::Acquire);
        let free = s.slots.len() - tail.wrapping_sub(head);
        let n = data.len().min(free);
        let n = n - n % channels.max(1);
        for (k, &v) in data[..n].iter().enumerate() {
            s.slots[tail.wrapping_add(k) & s.mask].store(v.to_bits(), Ordering::Relaxed);
        }
        s.tail.store(tail.wrapping_add(n), Ordering::Release);
        n
    }
}

#[cfg_attr(not(windows), allow(dead_code))]
impl RingConsumer {
    pub(crate) fn len(&self) -> usize {
        let s = &*self.shared;
        s.tail.load(Ordering::Acquire).wrapping_sub(s.head.load(Ordering::Relaxed))
    }

    /// Lit jusqu'à `out.len()` échantillons ; renvoie le nombre lu.
    pub(crate) fn pop(&mut self, out: &mut [f32]) -> usize {
        let s = &*self.shared;
        let head = s.head.load(Ordering::Relaxed);
        let n = out.len().min(self.len());
        for (k, o) in out[..n].iter_mut().enumerate() {
            *o = f32::from_bits(s.slots[head.wrapping_add(k) & s.mask].load(Ordering::Relaxed));
        }
        s.head.store(head.wrapping_add(n), Ordering::Release);
        n
    }
}

/// File d'attente de l'entrée, côté thread de sortie : la capture y dépose ce
/// qu'elle a reçu, la sortie en retire exactement un cycle.
///
/// Elle démarre sans marge (aucun retard ajouté si la capture et la sortie
/// battent au même rythme) ; à chaque famine, la marge grandit d'une
/// demi-période et la file se ré-amorce. Si elle se remplit trop (horloges
/// légèrement différentes), les trames les plus anciennes sont jetées.
pub(crate) struct InputFifo {
    buf: Vec<f32>,
    channels: usize,
    /// Capacité en trames.
    cap: usize,
    /// Première trame à lire, en trames.
    read: usize,
    /// Trames en attente.
    len: usize,
    /// Trames gardées en réserve au-delà d'un cycle.
    margin: usize,
    priming: bool,
    underruns: u32,
}

impl InputFifo {
    pub(crate) fn new(frames: usize, channels: usize) -> Self {
        let channels = channels.max(1);
        let cap = frames.max(16);
        Self { buf: vec![0.0; cap * channels], channels, cap, read: 0, len: 0, margin: 0, priming: true, underruns: 0 }
    }

    pub(crate) fn channels(&self) -> usize {
        self.channels
    }

    /// Trames en attente.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    /// Famines depuis la création.
    pub(crate) fn underruns(&self) -> u32 {
        self.underruns
    }

    /// Ajoute des trames entrelacées ; si la file déborde, les plus anciennes sont perdues.
    pub(crate) fn push(&mut self, data: &[f32]) {
        let ch = self.channels;
        for frame in data.chunks_exact(ch) {
            if self.len == self.cap {
                self.read = (self.read + 1) % self.cap;
                self.len -= 1;
            }
            let at = (self.read + self.len) % self.cap;
            self.buf[at * ch..(at + 1) * ch].copy_from_slice(frame);
            self.len += 1;
        }
    }

    /// Remplit `out` (trames entrelacées) ; renvoie l'attente, en trames, de la
    /// première trame rendue (sa latence dans la file). Sans allocation.
    pub(crate) fn pull(&mut self, out: &mut [f32]) -> usize {
        let ch = self.channels;
        let n = out.len() / ch;
        if self.priming {
            if self.len < n + self.margin {
                out.fill(0.0);
                return self.len;
            }
            self.priming = false;
        }
        if self.len < n {
            // Famine : silence, on garde ce qui est arrivé et on se donne plus de marge.
            self.underruns = self.underruns.saturating_add(1);
            self.margin = (self.margin + (n / 2).max(16)).min(self.cap / 4);
            self.priming = true;
            out.fill(0.0);
            return self.len;
        }
        // Trop d'avance (dérive d'horloge, rafale) : on revient à cycle + marge.
        let keep = n + self.margin;
        if self.len > keep + 2 * n.max(64) {
            let drop = self.len - keep;
            self.read = (self.read + drop) % self.cap;
            self.len -= drop;
        }
        let waited = self.len;
        for k in 0..n {
            let at = (self.read + k) % self.cap;
            out[k * ch..(k + 1) * ch].copy_from_slice(&self.buf[at * ch..(at + 1) * ch]);
        }
        self.read = (self.read + n) % self.cap;
        self.len -= n;
        waited
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_is_fifo_and_bounded() {
        let (mut tx, mut rx) = ring(4);
        assert_eq!(tx.push(&[1.0, 2.0, 3.0, 4.0, 5.0], 1), 4);
        let mut out = [0.0; 3];
        assert_eq!(rx.pop(&mut out), 3);
        assert_eq!(out, [1.0, 2.0, 3.0]);
        assert_eq!(tx.push(&[6.0, 7.0], 1), 2);
        let mut out = [0.0; 8];
        assert_eq!(rx.pop(&mut out), 3);
        assert_eq!(out[..3], [4.0, 6.0, 7.0]);
    }

    #[test]
    fn lockstep_capture_adds_no_latency() {
        let mut f = InputFifo::new(1024, 2);
        let mut out = [0.0; 2 * 64];
        for cycle in 0..10 {
            let block: Vec<f32> = (0..128).map(|i| (cycle * 1000 + i) as f32).collect();
            f.push(&block);
            // La première trame rendue n'attend que le cycle en cours.
            assert_eq!(f.pull(&mut out), 64);
            assert_eq!(out[0], (cycle * 1000) as f32);
        }
        assert_eq!(f.underruns(), 0);
    }

    #[test]
    fn bursty_capture_settles_with_a_margin() {
        // La capture arrive par paquets de deux cycles, un cycle sur deux.
        let mut f = InputFifo::new(4096, 1);
        let mut out = [0.0; 64];
        let mut next = 0.0f32;
        let mut expected = 0.0f32;
        let mut silent_after_settle = 0;
        for cycle in 0..200 {
            if cycle % 2 == 0 {
                let block: Vec<f32> = (0..128).map(|_| {
                    next += 1.0;
                    next
                }).collect();
                f.push(&block);
            }
            f.pull(&mut out);
            if out[0] != 0.0 {
                // Continuité : rien n'est perdu ni répété.
                for &s in &out {
                    expected += 1.0;
                    assert_eq!(s, expected);
                }
            } else if cycle > 20 {
                silent_after_settle += 1;
            }
        }
        assert!(f.underruns() <= 2, "{} famines", f.underruns());
        assert_eq!(silent_after_settle, 0);
    }

    #[test]
    fn drift_is_dropped_back_to_a_cycle() {
        let mut f = InputFifo::new(4096, 1);
        let mut out = [0.0; 64];
        f.push(&[0.5; 64 * 10]);
        let waited = f.pull(&mut out);
        assert_eq!(waited, 64);
        assert_eq!(f.len(), 0);
    }

    #[test]
    fn overflow_keeps_newest() {
        let mut f = InputFifo::new(16, 1);
        let data: Vec<f32> = (0..20).map(|i| i as f32).collect();
        f.push(&data);
        assert_eq!(f.len(), 16);
        let mut out = [0.0; 16];
        f.pull(&mut out);
        assert_eq!(out[0], 4.0);
    }
}
