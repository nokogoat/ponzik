//! Mixer à pistes fixes, comme FL Studio : la piste 0 est le master, chaque
//! piste a ses effets en insert, son volume, son pan, une sortie et des envois.

use ponzik_dsp::util::balance_gains;

use crate::effect::Effect;
use crate::meter::{PeakMeter, peak};

/// Master + 32 pistes.
pub const TRACKS: usize = 33;
pub const MASTER: usize = 0;
pub const INSERT_SLOTS: usize = 8;
pub const SEND_SLOTS: usize = 4;

pub struct Track {
    pub gain: f32,
    pub pan: f32,
    pub muted: bool,
    /// Piste de destination (le master par défaut).
    pub output: usize,
    /// Envois post-fader : (piste, niveau) ; niveau 0 = inactif.
    pub sends: [(usize, f32); SEND_SLOTS],
    pub inserts: [Option<Box<dyn Effect>>; INSERT_SLOTS],
    pub bypass: [bool; INSERT_SLOTS],
    /// Piste clé (entrée latérale) de chaque insert : son signal post-fader est
    /// passé à [`Effect::sidechain`] avant le traitement. Le routage doit la
    /// traiter avant cette piste (voir [`routing_order`]).
    pub sidechain: [Option<usize>; INSERT_SLOTS],
    pub(crate) l: Vec<f32>,
    pub(crate) r: Vec<f32>,
}

impl Track {
    fn new(max_block: usize) -> Self {
        Self {
            gain: 1.0,
            pan: 0.0,
            muted: false,
            output: MASTER,
            sends: [(MASTER, 0.0); SEND_SLOTS],
            inserts: std::array::from_fn(|_| None),
            bypass: [false; INSERT_SLOTS],
            sidechain: [None; INSERT_SLOTS],
            l: vec![0.0; max_block],
            r: vec![0.0; max_block],
        }
    }
}

pub struct Mixer {
    pub tracks: Vec<Track>,
    /// Ordre de traitement des pistes hors master (toute source avant sa destination).
    pub order: Box<[usize]>,
    /// Clé muette, pour une piste clé pas encore traitée (ordre périmé).
    silence: Vec<f32>,
}

/// Ordre par défaut : toutes les pistes vont directement au master.
pub fn default_order() -> Box<[usize]> {
    (1..TRACKS).rev().collect()
}

/// Tri topologique du routage : `None` si une boucle existe.
/// `edges[t]` liste les pistes à traiter après `t` : sa sortie, ses envois, et
/// les pistes dont un insert prend `t` comme clé d'entrée latérale.
pub fn routing_order(edges: &[Vec<usize>]) -> Option<Box<[usize]>> {
    let n = edges.len();
    let mut indegree = vec![0usize; n];
    for (src, outs) in edges.iter().enumerate() {
        for &d in outs {
            if d >= n || d == src {
                return None;
            }
            indegree[d] += 1;
        }
    }
    let mut ready: Vec<usize> = (0..n).filter(|&t| indegree[t] == 0).collect();
    let mut order = Vec::with_capacity(n);
    while let Some(t) = ready.pop() {
        order.push(t);
        for &d in &edges[t] {
            indegree[d] -= 1;
            if indegree[d] == 0 {
                ready.push(d);
            }
        }
    }
    if order.len() != n {
        return None;
    }
    Some(order.into_iter().filter(|&t| t != MASTER).collect())
}

/// Deux pistes distinctes empruntées en même temps.
fn pair(tracks: &mut [Track], a: usize, b: usize) -> (&mut Track, &mut Track) {
    debug_assert_ne!(a, b);
    if a < b {
        let (x, y) = tracks.split_at_mut(b);
        (&mut x[a], &mut y[0])
    } else {
        let (x, y) = tracks.split_at_mut(a);
        (&mut y[0], &mut x[b])
    }
}

impl Mixer {
    pub fn new(max_block: usize) -> Self {
        Self {
            tracks: (0..TRACKS).map(|_| Track::new(max_block)).collect(),
            order: default_order(),
            silence: vec![0.0; max_block],
        }
    }

    pub(crate) fn clear(&mut self, n: usize) {
        for t in &mut self.tracks {
            t.l[..n].fill(0.0);
            t.r[..n].fill(0.0);
        }
    }

    /// Entrée d'une piste, pour y ajouter le signal des canaux.
    pub(crate) fn input(&mut self, track: usize, n: usize) -> (&mut [f32], &mut [f32]) {
        let t = &mut self.tracks[track.min(TRACKS - 1)];
        (&mut t.l[..n], &mut t.r[..n])
    }

    /// Inserts de la piste `idx` ; `ready[k]` : la piste `k` est déjà traitée dans ce bloc.
    fn run_inserts(&mut self, idx: usize, n: usize, ready: &[bool; TRACKS]) {
        let Self { tracks, silence, .. } = self;
        for slot in 0..INSERT_SLOTS {
            let key = tracks[idx].sidechain[slot].filter(|&k| k < TRACKS && k != idx);
            let (t, key) = match key {
                Some(k) => {
                    let (t, kt) = pair(tracks, idx, k);
                    let key = if ready[k] { (&kt.l[..n], &kt.r[..n]) } else { (&silence[..n], &silence[..n]) };
                    (t, Some(key))
                }
                None => (&mut tracks[idx], None),
            };
            if t.bypass[slot] {
                continue;
            }
            let Some(fx) = t.inserts[slot].as_mut() else { continue };
            if let Some((kl, kr)) = key {
                fx.sidechain(kl, kr);
            }
            fx.process(&mut t.l[..n], &mut t.r[..n]);
        }
    }

    /// Traite toutes les pistes ; le résultat est dans le master.
    pub(crate) fn process(&mut self, n: usize, meters: &[PeakMeter]) {
        let mut ready = [false; TRACKS];
        for k in 0..self.order.len() {
            let idx = self.order[k];
            if idx == MASTER || idx >= TRACKS || ready[idx] {
                continue;
            }
            self.run_inserts(idx, n, &ready);
            ready[idx] = true;
            let t = &mut self.tracks[idx];
            let (gl, gr) = balance_gains(t.pan);
            let (gl, gr) = if t.muted { (0.0, 0.0) } else { (gl * t.gain, gr * t.gain) };
            for i in 0..n {
                t.l[i] *= gl;
                t.r[i] *= gr;
            }
            meters[idx].record(peak(&t.l[..n]), peak(&t.r[..n]));

            let output = if t.output < TRACKS && t.output != idx { t.output } else { MASTER };
            let sends = t.sends;
            {
                let (src, dst) = pair(&mut self.tracks, idx, output);
                for i in 0..n {
                    dst.l[i] += src.l[i];
                    dst.r[i] += src.r[i];
                }
            }
            for (target, amount) in sends {
                if amount > 0.0 && target < TRACKS && target != idx {
                    let (src, dst) = pair(&mut self.tracks, idx, target);
                    for i in 0..n {
                        dst.l[i] += src.l[i] * amount;
                        dst.r[i] += src.r[i] * amount;
                    }
                }
            }
        }

        self.run_inserts(MASTER, n, &ready);
        let m = &mut self.tracks[MASTER];
        let (gl, gr) = balance_gains(m.pan);
        let (gl, gr) = if m.muted { (0.0, 0.0) } else { (gl * m.gain, gr * m.gain) };
        for i in 0..n {
            // Garde-fou pour les oreilles et les enceintes.
            let (l, r) = (m.l[i] * gl, m.r[i] * gr);
            m.l[i] = if l.is_finite() { l.clamp(-1.0, 1.0) } else { 0.0 };
            m.r[i] = if r.is_finite() { r.clamp(-1.0, 1.0) } else { 0.0 };
        }
        meters[MASTER].record(peak(&m.l[..n]), peak(&m.r[..n]));
    }

    pub fn master(&self, n: usize) -> (&[f32], &[f32]) {
        let m = &self.tracks[MASTER];
        (&m.l[..n], &m.r[..n])
    }

    pub(crate) fn set_tempo(&mut self, bpm: f32) {
        for t in &mut self.tracks {
            for fx in t.inserts.iter_mut().flatten() {
                fx.set_tempo(bpm);
            }
        }
    }

    pub(crate) fn reset_effects(&mut self) {
        for t in &mut self.tracks {
            for fx in t.inserts.iter_mut().flatten() {
                fx.reset();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topological_order_and_cycles() {
        let mut edges = vec![Vec::new(); 4];
        edges[1] = vec![3];
        edges[2] = vec![0];
        edges[3] = vec![0];
        let order = routing_order(&edges).unwrap();
        let pos = |t| order.iter().position(|&x| x == t).unwrap();
        assert!(pos(1) < pos(3));
        assert!(!order.contains(&MASTER));

        edges[3] = vec![1];
        assert!(routing_order(&edges).is_none(), "1 → 3 → 1 est une boucle");
    }

    #[test]
    fn routing_sends_and_master() {
        let mut m = Mixer::new(8);
        let meters: Vec<PeakMeter> = (0..TRACKS).map(|_| PeakMeter::default()).collect();
        m.tracks[1].output = 2;
        m.tracks[1].sends[0] = (3, 0.5);
        m.tracks[2].gain = 0.5;
        let mut edges = vec![Vec::new(); TRACKS];
        edges[1] = vec![2, 3];
        for (t, e) in edges.iter_mut().enumerate().skip(2) {
            e.push(MASTER);
            let _ = t;
        }
        m.order = routing_order(&edges).unwrap();
        m.clear(4);
        m.input(1, 4).0.fill(0.4);
        m.process(4, &meters);
        // 0,4 → piste 2 (×0,5) = 0,2 ; envoi 0,4 × 0,5 = 0,2 vers la piste 3 ; total 0,4.
        assert!((m.master(4).0[0] - 0.4).abs() < 1e-6);
        assert_eq!(m.master(4).1[0], 0.0);
        assert_eq!(meters[2].take()[0], 0.2);
    }

    /// Piste 1 (signal 0,1) compressée avec la piste 2 comme clé ; renvoie la crête de la piste 1.
    fn keyed_peak(key_level: f32) -> f32 {
        let mut m = Mixer::new(256);
        let meters: Vec<PeakMeter> = (0..TRACKS).map(|_| PeakMeter::default()).collect();
        let mut fx = crate::registry::create_effect("compressor").unwrap();
        fx.set_param(0, -30.0);
        fx.set_param(1, 20.0);
        fx.set_param(2, 0.0);
        m.tracks[1].inserts[0] = Some(fx);
        m.tracks[1].sidechain[0] = Some(2);
        let mut edges = vec![Vec::new(); TRACKS];
        for (t, e) in edges.iter_mut().enumerate().skip(1) {
            e.push(MASTER);
            let _ = t;
        }
        edges[2].push(1);
        m.order = routing_order(&edges).unwrap();
        let pos = |t| m.order.iter().position(|&x| x == t).unwrap();
        assert!(pos(2) < pos(1), "la clé passe avant");
        for _ in 0..100 {
            m.clear(256);
            m.input(1, 256).0.fill(0.1);
            m.input(1, 256).1.fill(0.1);
            m.input(2, 256).0.fill(key_level);
            meters[1].take();
            m.process(256, &meters);
        }
        meters[1].take()[0]
    }

    #[test]
    fn sidechain_key_drives_the_compressor() {
        let loud = keyed_peak(1.0);
        assert!(loud < 0.02, "clé forte : {loud}");
        let silent = keyed_peak(0.0);
        assert!((silent - 0.1).abs() < 1e-4, "clé muette : {silent}");
    }

    #[test]
    fn sidechain_cycle_is_rejected() {
        let mut edges = vec![Vec::new(); 4];
        edges[1] = vec![2];
        edges[2] = vec![0];
        // Un insert de la piste 1 écoute la piste 2, qui sort dans la 1 : boucle.
        edges[2].push(1);
        assert!(routing_order(&edges).is_none());
        // Une piste ne peut pas être sa propre clé.
        let mut own = vec![Vec::new(); 3];
        own[1] = vec![0, 1];
        assert!(routing_order(&own).is_none());
    }
}
