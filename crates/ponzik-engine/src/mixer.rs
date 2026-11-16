//! Mixer à pistes fixes, comme FL Studio : la piste 0 est le master, chaque
//! piste a ses effets en insert, son volume, son pan, une sortie et des envois.
//!
//! Compensation de latence (PDC) : au début de chaque bloc, le mixer lit la
//! latence des inserts actifs ([`Effect::latency`]) et calcule, dans l'ordre de
//! routage, quand le signal de chaque piste arrive (`arrival`) puis en ressort
//! (`arrival + own`). L'entrée directe d'une piste (canaux, clips, écoute) est
//! retardée de `arrival`, et chaque liaison (sortie, envoi, clé d'entrée
//! latérale) du retard qui manque pour rejoindre le chemin le plus lent. Tout
//! arrive ainsi aligné au master. Les lignes de retard sont allouées à la
//! création ; elles ne sont écrites que lorsqu'un retard est demandé.
//!
//! Bus et VCA : un bus est une piste ordinaire dans laquelle sortent d'autres
//! pistes (ses inserts traitent leur somme). Un VCA ne fait passer aucun son :
//! son volume (et son muet) multiplie celui des pistes qu'il pilote
//! ([`Track::vca`]), automation comprise.

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
    /// Piste VCA qui pilote le volume de celle-ci : son gain et son muet
    /// s'appliquent en plus des siens.
    pub vca: Option<usize>,
    pub(crate) l: Vec<f32>,
    pub(crate) r: Vec<f32>,
    /// Retard de l'entrée directe (compensation).
    input_delay: Delay,
    /// Historique de la sortie post-fader, relu par les liaisons à compenser.
    output_delay: Delay,
}

impl Track {
    fn new(max_block: usize, max_latency: usize) -> Self {
        Self {
            gain: 1.0,
            pan: 0.0,
            muted: false,
            output: MASTER,
            sends: [(MASTER, 0.0); SEND_SLOTS],
            inserts: std::array::from_fn(|_| None),
            bypass: [false; INSERT_SLOTS],
            sidechain: [None; INSERT_SLOTS],
            vca: None,
            l: vec![0.0; max_block],
            r: vec![0.0; max_block],
            input_delay: Delay::new(max_block, max_latency),
            output_delay: Delay::new(max_block, max_latency),
        }
    }

    /// Latence cumulée des inserts actifs ; `reduced` : ceux qui anticipent
    /// sont court-circuités (écoute à faible latence), donc sans latence.
    fn insert_latency(&self, reduced: bool) -> usize {
        if reduced {
            return 0;
        }
        let active = self.inserts.iter().zip(&self.bypass).filter(|(_, bypass)| !**bypass);
        active.filter_map(|(fx, _)| fx.as_ref()).map(|fx| fx.latency()).sum()
    }

    /// Pistes qui reçoivent le signal de `idx` : sa sortie et ses envois actifs.
    fn destinations(&self, idx: usize) -> impl Iterator<Item = usize> + '_ {
        let output = if self.output < TRACKS && self.output != idx { self.output } else { MASTER };
        let sends = self.sends.iter().filter(move |&&(t, amount)| amount > 0.0 && t < TRACKS && t != idx);
        std::iter::once(output).chain(sends.map(|&(t, _)| t))
    }
}

/// Ligne de retard stéréo de la compensation : on écrit un bloc, puis on le
/// relit `d` échantillons dans le passé. L'historique non écrit (ligne au repos)
/// est lu comme du silence.
struct Delay {
    l: Vec<f32>,
    r: Vec<f32>,
    /// Prochaine case écrite.
    pos: usize,
    /// Échantillons récents écrits sans interruption.
    filled: usize,
}

impl Delay {
    fn new(max_block: usize, max_latency: usize) -> Self {
        // `vec!` de zéros : pages réservées mais pas touchées tant que la ligne dort.
        let len = if max_latency == 0 { 0 } else { max_block + max_latency };
        Self { l: vec![0.0; len], r: vec![0.0; len], pos: 0, filled: 0 }
    }

    /// Ligne au repos : l'historique est oublié.
    fn forget(&mut self) {
        self.filled = 0;
    }

    fn write(&mut self, l: &[f32], r: &[f32]) {
        let len = self.l.len();
        if len == 0 {
            return;
        }
        for (&a, &b) in l.iter().zip(r) {
            self.l[self.pos] = a;
            self.r[self.pos] = b;
            self.pos += 1;
            if self.pos == len {
                self.pos = 0;
            }
        }
        self.filled = (self.filled + l.len()).min(len);
    }

    /// Les `n` derniers échantillons écrits, retardés de `d` (`d + n` ≤ longueur).
    fn read(&self, d: usize, n: usize, mut f: impl FnMut(usize, f32, f32)) {
        let len = self.l.len();
        debug_assert!(d + n <= len);
        let mut j = (self.pos + len - n - d) % len;
        for i in 0..n {
            // Âge de l'échantillon lu : 0 pour le dernier écrit.
            let age = d + n - 1 - i;
            if age < self.filled {
                f(i, self.l[j], self.r[j]);
            } else {
                f(i, 0.0, 0.0);
            }
            j += 1;
            if j == len {
                j = 0;
            }
        }
    }
}

pub struct Mixer {
    pub tracks: Vec<Track>,
    /// Ordre de traitement des pistes hors master (toute source avant sa destination).
    pub order: Box<[usize]>,
    /// Clé muette, pour une piste clé pas encore traitée (ordre périmé).
    silence: Vec<f32>,
    /// Clé d'entrée latérale retardée (compensation).
    key_l: Vec<f32>,
    key_r: Vec<f32>,
    /// Plus grand retard compensable, en échantillons.
    max_latency: usize,
    /// Latence à l'entrée de chaque piste (chemin le plus lent qui y mène).
    arrival: [usize; TRACKS],
    /// Latence des inserts actifs de chaque piste.
    own: [usize; TRACKS],
    /// La sortie de la piste est relue en retard par au moins une liaison.
    history: [bool; TRACKS],
    /// Latence totale au master, inserts du master compris.
    latency: usize,
    /// Écoute à faible latence : pistes dont les inserts qui anticipent sont
    /// court-circuités et dont le signal n'est pas retardé par la compensation.
    pub(crate) reduced: [bool; TRACKS],
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
    /// `max_latency` : plus grand retard compensé, en échantillons (0 : pas de compensation).
    pub fn new(max_block: usize, max_latency: usize) -> Self {
        Self {
            tracks: (0..TRACKS).map(|_| Track::new(max_block, max_latency)).collect(),
            order: default_order(),
            silence: vec![0.0; max_block],
            key_l: vec![0.0; max_block],
            key_r: vec![0.0; max_block],
            max_latency,
            arrival: [0; TRACKS],
            own: [0; TRACKS],
            history: [false; TRACKS],
            latency: 0,
            reduced: [false; TRACKS],
        }
    }

    /// Latence totale au master, en échantillons, mesurée au dernier bloc :
    /// retard du chemin le plus lent, inserts du master compris.
    pub fn latency(&self) -> usize {
        self.latency
    }

    /// Retard qui manque à un signal de latence `have` pour rejoindre `want`.
    fn gap(&self, want: usize, have: usize) -> usize {
        want.saturating_sub(have).min(self.max_latency)
    }

    /// Latence de sortie de la piste (entrée + inserts).
    fn out_latency(&self, idx: usize) -> usize {
        self.arrival[idx] + self.own[idx]
    }

    /// Latences du bloc à venir, dans l'ordre de routage (sources avant destinations).
    pub(crate) fn plan_latency(&mut self) {
        self.arrival = [0; TRACKS];
        self.own = [0; TRACKS];
        self.history = [false; TRACKS];
        let mut seen = [false; TRACKS];
        for k in 0..self.order.len() {
            let idx = self.order[k];
            if idx == MASTER || idx >= TRACKS || seen[idx] {
                continue;
            }
            seen[idx] = true;
            let t = &self.tracks[idx];
            self.own[idx] = t.insert_latency(self.reduced[idx]);
            let out = self.out_latency(idx);
            for dst in t.destinations(idx) {
                self.arrival[dst] = self.arrival[dst].max(out);
            }
        }
        self.own[MASTER] = self.tracks[MASTER].insert_latency(self.reduced[MASTER]);
        seen[MASTER] = true;

        // Une sortie n'est historisée que si une liaison la relit en retard.
        for idx in (0..TRACKS).filter(|&t| seen[t]) {
            let t = &self.tracks[idx];
            if idx != MASTER {
                let out = self.out_latency(idx);
                self.history[idx] |= t.destinations(idx).any(|dst| self.gap(self.arrival[dst], out) > 0);
            }
            let mut at = self.arrival[idx];
            for slot in 0..INSERT_SLOTS {
                let Some(fx) = t.inserts[slot].as_ref().filter(|_| !t.bypass[slot]) else { continue };
                if self.reduced[idx] && fx.latency() > 0 {
                    continue;
                }
                if let Some(k) = t.sidechain[slot].filter(|&k| k < TRACKS && k != idx && k != MASTER)
                    && self.gap(at, self.out_latency(k)) > 0
                {
                    self.history[k] = true;
                }
                at += fx.latency();
            }
        }
        self.latency = self.out_latency(MASTER);
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
        // Latence du signal de la piste à l'entrée de l'insert courant.
        let mut at = self.arrival[idx];
        for slot in 0..INSERT_SLOTS {
            let key = self.tracks[idx].sidechain[slot].filter(|&k| k < TRACKS && k != idx);
            let delay = key.map_or(0, |k| self.gap(at, self.out_latency(k)));
            let Self { tracks, silence, key_l, key_r, .. } = self;
            let (t, key) = match key {
                Some(k) => {
                    let (t, kt) = pair(tracks, idx, k);
                    let key = if !ready[k] {
                        (&silence[..n], &silence[..n])
                    } else if delay > 0 {
                        // Clé en avance sur le signal qu'elle pilote : on la retarde.
                        kt.output_delay.read(delay, n, |i, l, r| {
                            key_l[i] = l;
                            key_r[i] = r;
                        });
                        (&key_l[..n], &key_r[..n])
                    } else {
                        (&kt.l[..n], &kt.r[..n])
                    };
                    (t, Some(key))
                }
                None => (&mut tracks[idx], None),
            };
            if t.bypass[slot] {
                continue;
            }
            let Some(fx) = t.inserts[slot].as_mut() else { continue };
            if self.reduced[idx] && fx.latency() > 0 {
                continue;
            }
            at += fx.latency();
            if let Some((kl, kr)) = key {
                fx.sidechain(kl, kr);
            }
            fx.process(&mut t.l[..n], &mut t.r[..n]);
        }
    }

    /// Ajoute la sortie de `src` à `dst` (× `amount`), retardée pour rejoindre
    /// le chemin le plus lent qui arrive à `dst`.
    fn route(&mut self, src: usize, dst: usize, amount: f32, n: usize) {
        // Trajet d'écoute à faible latence : jamais retardé.
        let delay = if self.reduced[src] { 0 } else { self.gap(self.arrival[dst], self.out_latency(src)) };
        let (s, d) = pair(&mut self.tracks, src, dst);
        if delay == 0 {
            for i in 0..n {
                d.l[i] += s.l[i] * amount;
                d.r[i] += s.r[i] * amount;
            }
        } else {
            let Track { l, r, .. } = d;
            s.output_delay.read(delay, n, |i, a, b| {
                l[i] += a * amount;
                r[i] += b * amount;
            });
        }
    }

    /// Retarde l'entrée directe de chaque piste (canaux, clips, écoute, métronome)
    /// de la latence des pistes qui y arrivent.
    fn delay_inputs(&mut self, n: usize) {
        for idx in 0..TRACKS {
            let delay = if self.reduced[idx] { 0 } else { self.gap(self.arrival[idx], 0) };
            let Track { l, r, input_delay, .. } = &mut self.tracks[idx];
            if delay == 0 {
                input_delay.forget();
                continue;
            }
            input_delay.write(&l[..n], &r[..n]);
            input_delay.read(delay, n, |i, a, b| {
                l[i] = a;
                r[i] = b;
            });
        }
    }

    /// Traite toutes les pistes ; le résultat est dans le master.
    pub(crate) fn process(&mut self, n: usize, meters: &[PeakMeter]) {
        self.plan_latency();
        self.delay_inputs(n);
        let mut ready = [false; TRACKS];
        for k in 0..self.order.len() {
            let idx = self.order[k];
            if idx == MASTER || idx >= TRACKS || ready[idx] {
                continue;
            }
            self.run_inserts(idx, n, &ready);
            ready[idx] = true;
            let vca = self.vca_gain(idx);
            let t = &mut self.tracks[idx];
            let (gl, gr) = balance_gains(t.pan);
            let (gl, gr) = if t.muted { (0.0, 0.0) } else { (gl * t.gain * vca, gr * t.gain * vca) };
            for i in 0..n {
                t.l[i] *= gl;
                t.r[i] *= gr;
            }
            meters[idx].record(peak(&t.l[..n]), peak(&t.r[..n]));
            if self.history[idx] {
                let Track { l, r, output_delay, .. } = t;
                output_delay.write(&l[..n], &r[..n]);
            } else {
                t.output_delay.forget();
            }

            let output = if t.output < TRACKS && t.output != idx { t.output } else { MASTER };
            let sends = t.sends;
            self.route(idx, output, 1.0, n);
            for (target, amount) in sends {
                if amount > 0.0 && target < TRACKS && target != idx {
                    self.route(idx, target, amount, n);
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

    /// Facteur du VCA qui pilote la piste (1 sans VCA, 0 s'il est muet).
    fn vca_gain(&self, idx: usize) -> f32 {
        match self.tracks[idx].vca.filter(|&v| v < TRACKS && v != idx && v != MASTER) {
            Some(v) if self.tracks[v].muted => 0.0,
            Some(v) => self.tracks[v].gain,
            None => 1.0,
        }
    }

    pub fn master(&self, n: usize) -> (&[f32], &[f32]) {
        let m = &self.tracks[MASTER];
        (&m.l[..n], &m.r[..n])
    }

    /// Sortie du master après traitement, pour y ajouter un signal hors mixage.
    pub(crate) fn master_mut(&mut self, n: usize) -> (&mut [f32], &mut [f32]) {
        let m = &mut self.tracks[MASTER];
        (&mut m.l[..n], &mut m.r[..n])
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
            t.input_delay.forget();
            t.output_delay.forget();
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
        let mut m = Mixer::new(8, 1_024);
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
        let mut m = Mixer::new(256, 4_800);
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

    // --- Compensation de latence ---

    const SR: f32 = 48_000.0;

    fn meters() -> Vec<PeakMeter> {
        (0..TRACKS).map(|_| PeakMeter::default()).collect()
    }

    fn limiter() -> Box<dyn Effect> {
        let mut fx = crate::registry::create_effect("limiter").unwrap();
        fx.set_sample_rate(SR);
        fx
    }

    /// Ordre de routage déduit des sorties, envois et clés des pistes.
    fn reorder(m: &mut Mixer) {
        let mut edges = vec![Vec::new(); TRACKS];
        for (idx, t) in m.tracks.iter().enumerate().skip(1) {
            edges[idx].extend(t.destinations(idx));
            for k in t.sidechain.iter().flatten() {
                edges[*k].push(idx);
            }
        }
        m.order = routing_order(&edges).unwrap();
    }

    /// Impulsion de 0,25 au premier échantillon des pistes `inputs` ; renvoie le master gauche.
    fn impulse(m: &mut Mixer, inputs: &[usize], frames: usize, block: usize) -> Vec<f32> {
        let meters = meters();
        let mut out = Vec::new();
        let mut first = true;
        while out.len() < frames {
            let n = block.min(frames - out.len());
            m.clear(n);
            if first {
                for &t in inputs {
                    m.input(t, n).0[0] = 0.25;
                }
                first = false;
            }
            m.process(n, &meters);
            out.extend_from_slice(m.master(n).0);
        }
        out
    }

    /// Indice de la crête.
    fn argmax(x: &[f32]) -> usize {
        (0..x.len()).max_by(|&a, &b| x[a].abs().total_cmp(&x[b].abs())).unwrap()
    }

    #[test]
    fn limited_and_dry_tracks_arrive_together() {
        let lat = limiter().latency();
        assert!(lat > 64, "l'anticipation dépasse un bloc : {lat}");
        let mut m = Mixer::new(64, 4_800);
        m.tracks[1].inserts[0] = Some(limiter());
        reorder(&mut m);
        let limited = impulse(&mut m, &[1], 2_000, 64);
        let dry = impulse(&mut m, &[2], 2_000, 64);
        assert_eq!(m.latency(), lat);
        assert_eq!(argmax(&limited), lat);
        assert_eq!(argmax(&dry), lat, "la piste sans limiteur est retardée d'autant");
        assert!((dry[lat] - 0.25).abs() < 1e-6 && (limited[lat] - 0.25).abs() < 1e-6);
        assert_eq!(dry.iter().filter(|s| **s != 0.0).count(), 1, "rien d'autre que l'impulsion");

        // Limiteur contourné : plus de latence, plus de retard.
        m.tracks[1].bypass[0] = true;
        let dry = impulse(&mut m, &[2], 256, 64);
        assert_eq!((m.latency(), argmax(&dry)), (0, 0));
    }

    #[test]
    fn send_to_a_latent_bus_is_compensated() {
        // Piste 1 → master en direct et envoi vers le bus 3 limité ; le direct attend le bus.
        let lat = limiter().latency();
        let mut m = Mixer::new(64, 4_800);
        m.tracks[1].sends[0] = (3, 0.5);
        m.tracks[3].inserts[0] = Some(limiter());
        reorder(&mut m);
        let out = impulse(&mut m, &[1], 2_000, 64);
        assert_eq!(m.latency(), lat);
        assert!((out[lat] - 0.375).abs() < 1e-6, "direct 0,25 + envoi 0,125 au même échantillon : {}", out[lat]);
        assert_eq!(out.iter().filter(|s| **s != 0.0).count(), 1);
    }

    #[test]
    fn chained_buses_add_their_latency() {
        // Piste 1 limitée → bus 2 limité → master ; piste 3 sèche → master.
        let lat = limiter().latency();
        let mut m = Mixer::new(64, 4_800);
        m.tracks[1].inserts[0] = Some(limiter());
        m.tracks[1].output = 2;
        m.tracks[2].inserts[0] = Some(limiter());
        reorder(&mut m);
        assert_eq!(argmax(&impulse(&mut m, &[1], 2_000, 64)), 2 * lat);
        assert_eq!(argmax(&impulse(&mut m, &[3], 2_000, 64)), 2 * lat);
        // L'entrée directe du bus 2 attend aussi la piste 1.
        assert_eq!(argmax(&impulse(&mut m, &[2], 2_000, 64)), 2 * lat);
        assert_eq!(m.latency(), 2 * lat);
    }

    /// Remplace le signal par la clé reçue.
    struct KeyEcho([f32; 64], [f32; 64]);

    impl crate::param::Params for KeyEcho {}

    impl Effect for KeyEcho {
        fn kind(&self) -> &'static str {
            "key_echo"
        }

        fn set_sample_rate(&mut self, _: f32) {}

        fn sidechain(&mut self, l: &[f32], r: &[f32]) {
            self.0[..l.len()].copy_from_slice(l);
            self.1[..r.len()].copy_from_slice(r);
        }

        fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
            l.copy_from_slice(&self.0[..l.len()]);
            r.copy_from_slice(&self.1[..r.len()]);
        }
    }

    #[test]
    fn sidechain_key_is_aligned_with_the_keyed_signal() {
        // Piste 1 : limiteur puis un insert piloté par la piste 2 (sèche) ; la clé
        // doit arriver à l'insert en même temps que le signal limité.
        let lat = limiter().latency();
        let mut m = Mixer::new(64, 4_800);
        m.tracks[1].inserts[0] = Some(limiter());
        m.tracks[1].inserts[1] = Some(Box::new(KeyEcho([0.0; 64], [0.0; 64])));
        m.tracks[1].sidechain[1] = Some(2);
        reorder(&mut m);
        let out = impulse(&mut m, &[2], 2_000, 64);
        // Piste 2 au master (retardée de `lat`) + écho de la clé par la piste 1 (à `lat`).
        assert!((out[lat] - 0.5).abs() < 1e-6, "{}", out[lat]);
        assert_eq!(out.iter().filter(|s| **s != 0.0).count(), 1);
    }

    #[test]
    fn bus_sums_two_tracks_through_its_effect() {
        // Pistes 1 et 2 → bus 5 (gain ½, Utilitaire à +6 dB) → master.
        let mut m = Mixer::new(64, 4_800);
        let mut util = crate::registry::create_effect("utility").unwrap();
        util.set_sample_rate(SR);
        let gain_id = (0..16).find(|&id| util.param_info(id).is_some_and(|p| p.name == "Gain")).unwrap();
        util.set_param(gain_id, 6.0206);
        m.tracks[5].inserts[0] = Some(util);
        m.tracks[5].gain = 0.5;
        m.tracks[1].output = 5;
        m.tracks[2].output = 5;
        m.tracks[2].gain = 0.5;
        reorder(&mut m);
        let pos = |t| m.order.iter().position(|&x| x == t).unwrap();
        assert!(pos(1) < pos(5) && pos(2) < pos(5));
        let meters = meters();
        let mut out = 0.0;
        for _ in 0..200 {
            m.clear(64);
            for t in [1, 2] {
                let (l, r) = m.input(t, 64);
                l.fill(0.2);
                r.fill(0.2);
            }
            m.process(64, &meters);
            out = m.master(64).0[63];
        }
        // (0,2 + 0,1) × 2 × ½ = 0,3.
        assert!((out - 0.3).abs() < 1e-3, "{out}");
        // Bus coupé : plus rien n'arrive au master.
        m.tracks[5].muted = true;
        m.clear(64);
        m.input(1, 64).0.fill(0.2);
        m.process(64, &meters);
        assert_eq!(m.master(64).0[63], 0.0);
    }

    #[test]
    fn latent_bus_effect_is_compensated_against_dry_tracks() {
        // Pistes 1 et 2 → bus 4 limité ; piste 3 sèche au master : tout arrive ensemble.
        let lat = limiter().latency();
        let mut m = Mixer::new(64, 4_800);
        m.tracks[1].output = 4;
        m.tracks[2].output = 4;
        m.tracks[4].inserts[0] = Some(limiter());
        reorder(&mut m);
        let out = impulse(&mut m, &[1, 2, 3], 2_000, 64);
        assert_eq!(m.latency(), lat);
        assert!((out[lat] - 0.75).abs() < 1e-6, "{}", out[lat]);
        assert_eq!(out.iter().filter(|s| **s != 0.0).count(), 1);
    }

    #[test]
    fn vca_scales_its_tracks_without_carrying_audio() {
        let mut m = Mixer::new(8, 0);
        let meters = meters();
        m.tracks[1].vca = Some(6);
        m.tracks[2].vca = Some(6);
        m.tracks[6].gain = 0.25;
        reorder(&mut m);
        let run = |m: &mut Mixer| {
            m.clear(8);
            m.input(1, 8).0.fill(0.4);
            m.input(2, 8).0.fill(0.4);
            m.input(3, 8).0.fill(0.1);
            m.process(8, &meters);
            m.master(8).0[0]
        };
        // (0,4 + 0,4) × ¼ + 0,1 (piste 3 hors VCA).
        assert!((run(&mut m) - 0.3).abs() < 1e-6);
        assert_eq!(meters[6].take()[0], 0.0, "le VCA ne fait passer aucun son");
        m.tracks[6].muted = true;
        assert!((run(&mut m) - 0.1).abs() < 1e-6, "VCA muet : ses pistes se taisent");
        // Un VCA qui se pilote lui-même ou le master est ignoré.
        m.tracks[6].muted = false;
        m.tracks[6].vca = Some(6);
        m.tracks[3].vca = Some(MASTER);
        assert!((run(&mut m) - 0.3).abs() < 1e-6);
    }

    #[test]
    fn without_latency_nothing_is_delayed() {
        let mut m = Mixer::new(64, 4_800);
        m.tracks[1].sends[0] = (3, 0.5);
        m.tracks[1].output = 2;
        reorder(&mut m);
        let out = impulse(&mut m, &[1, 4], 128, 64);
        assert!((out[0] - 0.625).abs() < 1e-6);
        assert_eq!(m.latency(), 0);
        assert!(m.tracks.iter().all(|t| t.input_delay.filled == 0 && t.output_delay.filled == 0));
    }
}
