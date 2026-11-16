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

use std::time::Instant;

use crate::effect::Effect;
use crate::meter::{PeakMeter, peak};
use crate::modulation::{MOD_CHUNK, ModClock, ModInput, ModSet};
use crate::pool::{Graph, WorkerPool};

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
    /// Modulators of the track's inserts (rack macros and devices included).
    pub mods: Option<Box<ModSet>>,
    pub(crate) l: Vec<f32>,
    pub(crate) r: Vec<f32>,
    /// Retard de l'entrée directe (compensation).
    input_delay: Delay,
    /// Historique de la sortie post-fader, relu par les liaisons à compenser.
    output_delay: Delay,
    /// Clé d'entrée latérale retardée (compensation).
    key_l: Vec<f32>,
    key_r: Vec<f32>,
    /// Temps de calcul accumulé (ns) de la piste et de chaque insert.
    pub(crate) cpu_ns: u64,
    pub(crate) insert_ns: [u64; INSERT_SLOTS],
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
            mods: None,
            l: vec![0.0; max_block],
            r: vec![0.0; max_block],
            input_delay: Delay::new(max_block, max_latency),
            output_delay: Delay::new(max_block, max_latency),
            key_l: vec![0.0; max_block],
            key_r: vec![0.0; max_block],
            cpu_ns: 0,
            insert_ns: [0; INSERT_SLOTS],
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

/// Compensation de latence du bloc en cours, calculée par [`Mixer::plan_latency`].
struct Latency {
    /// Plus grand retard compensable, en échantillons.
    max: usize,
    /// Latence à l'entrée de chaque piste (chemin le plus lent qui y mène).
    arrival: [usize; TRACKS],
    /// Latence des inserts actifs de chaque piste.
    own: [usize; TRACKS],
    /// La sortie de la piste est relue en retard par au moins une liaison.
    history: [bool; TRACKS],
    /// Latence totale au master, inserts du master compris.
    total: usize,
}

impl Latency {
    /// Retard qui manque à un signal de latence `have` pour rejoindre `want`.
    fn gap(&self, want: usize, have: usize) -> usize {
        want.saturating_sub(have).min(self.max)
    }

    /// Latence de sortie de la piste (entrée + inserts).
    fn out(&self, idx: usize) -> usize {
        self.arrival[idx] + self.own[idx]
    }
}

/// Ce que le traitement d'une piste lit en dehors des pistes elles-mêmes.
struct Ctx<'a> {
    n: usize,
    lat: &'a Latency,
    reduced: &'a [bool; TRACKS],
    silence: &'a [f32],
    meters: &'a [PeakMeter],
    clock: ModClock,
}

/// Pistes partagées entre les threads du bloc : chaque tâche n'emprunte en
/// écriture que sa propre piste, et ne lit que des pistes déjà traitées.
#[derive(Clone, Copy)]
struct Tracks(*mut Track);

// SAFETY: l'ordonnancement (ordre de routage, graphe de dépendances) garantit
// qu'une piste n'est jamais écrite pendant qu'un autre thread y accède.
unsafe impl Send for Tracks {}
unsafe impl Sync for Tracks {}

/// Pas de position : la piste n'est pas dans l'ordre de routage.
const NONE: usize = usize::MAX;
/// Liaisons d'un bloc au plus : une sortie et des envois par piste.
const MAX_ROUTES: usize = TRACKS * (1 + SEND_SLOTS);

/// Plan du traitement parallèle, refait à chaque bloc sans allocation.
struct Plan {
    graph: Graph,
    /// Piste de chaque tâche, dans l'ordre de routage.
    task_track: [usize; TRACKS],
    /// Position de chaque piste dans l'ordre (`NONE` : absente, `TRACKS` : master).
    pos: [usize; TRACKS],
    /// Liaisons qui arrivent à chaque piste, dans l'ordre où le chemin de
    /// référence les additionne : `routes[start[t]..start[t + 1]]` = (source, niveau).
    start: [usize; TRACKS + 1],
    routes: [(usize, f32); MAX_ROUTES],
    vca: [f32; TRACKS],
}

impl Plan {
    fn routes_into(&self, dst: usize) -> &[(usize, f32)] {
        &self.routes[self.start[dst]..self.start[dst + 1]]
    }
}

pub struct Mixer {
    pub tracks: Vec<Track>,
    /// Ordre de traitement des pistes hors master (toute source avant sa destination).
    pub order: Box<[usize]>,
    /// Clé muette, pour une piste clé pas encore traitée (ordre périmé).
    silence: Vec<f32>,
    lat: Latency,
    /// Écoute à faible latence : pistes dont les inserts qui anticipent sont
    /// court-circuités et dont le signal n'est pas retardé par la compensation.
    pub(crate) reduced: [bool; TRACKS],
    plan: Box<Plan>,
    /// Transport of the block, for the modulators.
    pub(crate) clock: ModClock,
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

/// Ajoute la sortie de `s` à `d` (× `amount`), retardée de `delay`.
fn add_route(s: &Track, d: &mut Track, delay: usize, amount: f32, n: usize) {
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

/// Ajoute la sortie de `src` à `dst` (× `amount`), retardée pour rejoindre
/// le chemin le plus lent qui arrive à `dst`.
///
/// # Safety
/// `src` et `dst` sont distinctes, personne d'autre n'accède à `dst` ni n'écrit `src`.
unsafe fn route(tracks: Tracks, src: usize, dst: usize, amount: f32, cx: &Ctx) {
    debug_assert_ne!(src, dst);
    // Trajet d'écoute à faible latence : jamais retardé.
    let delay = if cx.reduced[src] { 0 } else { cx.lat.gap(cx.lat.arrival[dst], cx.lat.out(src)) };
    // SAFETY: voir la fonction.
    let (s, d) = unsafe { (&*tracks.0.add(src), &mut *tracks.0.add(dst)) };
    add_route(s, d, delay, amount, cx.n);
}

/// Inserts de la piste `idx` ; `ready(k)` : la piste `k` est déjà traitée dans ce bloc.
///
/// # Safety
/// La piste `idx` est à l'appelant seul ; les pistes que `ready` accepte ne sont plus écrites.
unsafe fn run_inserts(tracks: Tracks, idx: usize, cx: &Ctx, ready: impl Fn(usize) -> bool) {
    // SAFETY: voir la fonction.
    let modulated = unsafe { (*tracks.0.add(idx)).mods.as_ref().is_some_and(|m| !m.is_empty()) };
    if !modulated {
        // SAFETY: voir la fonction.
        return unsafe { run_insert_range(tracks, idx, cx, &ready, 0, cx.n) };
    }
    // Modulated track: the inserts run in chunks, the modulators updated before each.
    let mut a = 0;
    while a < cx.n {
        let b = (a + MOD_CHUNK).min(cx.n);
        {
            // SAFETY: voir la fonction ; l'emprunt se termine avant les inserts.
            let Track { mods, inserts, l, r, .. } = unsafe { &mut *tracks.0.add(idx) };
            if let Some(mods) = mods.as_mut() {
                let input = ModInput::Audio(&l[a..b], &r[a..b]);
                mods.run(&cx.clock.after(a), b - a, &input, |slot, id, v| {
                    if let Some(Some(fx)) = inserts.get_mut(slot) {
                        fx.set_param(id, v);
                    }
                });
            }
        }
        // SAFETY: voir la fonction.
        unsafe { run_insert_range(tracks, idx, cx, &ready, a, b) };
        a = b;
    }
}

/// Inserts of track `idx` over the frames `a..b` of the block.
///
/// # Safety
/// Comme [`run_inserts`].
unsafe fn run_insert_range(tracks: Tracks, idx: usize, cx: &Ctx, ready: &impl Fn(usize) -> bool, a: usize, b: usize) {
    let n = cx.n;
    // SAFETY: voir la fonction.
    let Track { inserts, bypass, sidechain, l, r, key_l, key_r, insert_ns, .. } = unsafe { &mut *tracks.0.add(idx) };
    // Latence du signal de la piste à l'entrée de l'insert courant.
    let mut at = cx.lat.arrival[idx];
    for slot in 0..INSERT_SLOTS {
        let key = sidechain[slot].filter(|&k| k < TRACKS && k != idx);
        let delay = key.map_or(0, |k| cx.lat.gap(at, cx.lat.out(k)));
        if bypass[slot] {
            continue;
        }
        let Some(fx) = inserts[slot].as_mut() else { continue };
        if cx.reduced[idx] && fx.latency() > 0 {
            continue;
        }
        at += fx.latency();
        let started = Instant::now();
        if let Some(k) = key {
            let (kl, kr): (&[f32], &[f32]) = if !ready(k) {
                (&cx.silence[a..b], &cx.silence[a..b])
            } else {
                // SAFETY: piste clé déjà traitée, lue seulement.
                let kt = unsafe { &*tracks.0.add(k) };
                if delay > 0 {
                    // Clé en avance sur le signal qu'elle pilote : on la retarde.
                    // Frames a..b of the block: the last n - a written, minus the n - b after.
                    kt.output_delay.read(delay + (n - b), b - a, |i, x, y| {
                        key_l[a + i] = x;
                        key_r[a + i] = y;
                    });
                    (&key_l[a..b], &key_r[a..b])
                } else {
                    (&kt.l[a..b], &kt.r[a..b])
                }
            };
            fx.sidechain(kl, kr);
        }
        fx.process(&mut l[a..b], &mut r[a..b]);
        insert_ns[slot] += started.elapsed().as_nanos() as u64;
    }
}

/// Inserts, fader, crête et historique d'une piste hors master.
///
/// # Safety
/// Comme [`run_inserts`].
unsafe fn process_track(tracks: Tracks, idx: usize, cx: &Ctx, vca: f32, ready: impl Fn(usize) -> bool) {
    let started = Instant::now();
    let n = cx.n;
    // SAFETY: voir la fonction.
    unsafe { run_inserts(tracks, idx, cx, ready) };
    // SAFETY: voir la fonction.
    let t = unsafe { &mut *tracks.0.add(idx) };
    let (gl, gr) = balance_gains(t.pan);
    let (gl, gr) = if t.muted { (0.0, 0.0) } else { (gl * t.gain * vca, gr * t.gain * vca) };
    for i in 0..n {
        t.l[i] *= gl;
        t.r[i] *= gr;
    }
    cx.meters[idx].record(peak(&t.l[..n]), peak(&t.r[..n]));
    if cx.lat.history[idx] {
        let Track { l, r, output_delay, .. } = t;
        output_delay.write(&l[..n], &r[..n]);
    } else {
        t.output_delay.forget();
    }
    t.cpu_ns += started.elapsed().as_nanos() as u64;
}

/// Inserts et fader du master, garde-fou compris.
///
/// # Safety
/// Comme [`run_inserts`], pour le master.
unsafe fn process_master(tracks: Tracks, cx: &Ctx, ready: impl Fn(usize) -> bool) {
    let started = Instant::now();
    let n = cx.n;
    // SAFETY: voir la fonction.
    unsafe { run_inserts(tracks, MASTER, cx, ready) };
    // SAFETY: voir la fonction.
    let m = unsafe { &mut *tracks.0.add(MASTER) };
    let (gl, gr) = balance_gains(m.pan);
    let (gl, gr) = if m.muted { (0.0, 0.0) } else { (gl * m.gain, gr * m.gain) };
    for i in 0..n {
        // Garde-fou pour les oreilles et les enceintes.
        let (l, r) = (m.l[i] * gl, m.r[i] * gr);
        m.l[i] = if l.is_finite() { l.clamp(-1.0, 1.0) } else { 0.0 };
        m.r[i] = if r.is_finite() { r.clamp(-1.0, 1.0) } else { 0.0 };
    }
    cx.meters[MASTER].record(peak(&m.l[..n]), peak(&m.r[..n]));
    m.cpu_ns += started.elapsed().as_nanos() as u64;
}

impl Mixer {
    /// `max_latency` : plus grand retard compensé, en échantillons (0 : pas de compensation).
    pub fn new(max_block: usize, max_latency: usize) -> Self {
        Self {
            tracks: (0..TRACKS).map(|_| Track::new(max_block, max_latency)).collect(),
            order: default_order(),
            silence: vec![0.0; max_block],
            lat: Latency {
                max: max_latency,
                arrival: [0; TRACKS],
                own: [0; TRACKS],
                history: [false; TRACKS],
                total: 0,
            },
            reduced: [false; TRACKS],
            plan: Box::new(Plan {
                graph: Graph::new(),
                task_track: [0; TRACKS],
                pos: [NONE; TRACKS],
                start: [0; TRACKS + 1],
                routes: [(0, 0.0); MAX_ROUTES],
                vca: [1.0; TRACKS],
            }),
            clock: ModClock::default(),
        }
    }

    /// Latence totale au master, en échantillons, mesurée au dernier bloc :
    /// retard du chemin le plus lent, inserts du master compris.
    pub fn latency(&self) -> usize {
        self.lat.total
    }

    /// Latences du bloc à venir, dans l'ordre de routage (sources avant destinations).
    pub(crate) fn plan_latency(&mut self) {
        let lat = &mut self.lat;
        lat.arrival = [0; TRACKS];
        lat.own = [0; TRACKS];
        lat.history = [false; TRACKS];
        let mut seen = [false; TRACKS];
        for k in 0..self.order.len() {
            let idx = self.order[k];
            if idx == MASTER || idx >= TRACKS || seen[idx] {
                continue;
            }
            seen[idx] = true;
            let t = &self.tracks[idx];
            lat.own[idx] = t.insert_latency(self.reduced[idx]);
            let out = lat.out(idx);
            for dst in t.destinations(idx) {
                lat.arrival[dst] = lat.arrival[dst].max(out);
            }
        }
        lat.own[MASTER] = self.tracks[MASTER].insert_latency(self.reduced[MASTER]);
        seen[MASTER] = true;

        // Une sortie n'est historisée que si une liaison la relit en retard.
        for idx in (0..TRACKS).filter(|&t| seen[t]) {
            let t = &self.tracks[idx];
            if idx != MASTER {
                let out = lat.out(idx);
                lat.history[idx] |= t.destinations(idx).any(|dst| lat.gap(lat.arrival[dst], out) > 0);
            }
            let mut at = lat.arrival[idx];
            for slot in 0..INSERT_SLOTS {
                let Some(fx) = t.inserts[slot].as_ref().filter(|_| !t.bypass[slot]) else { continue };
                if self.reduced[idx] && fx.latency() > 0 {
                    continue;
                }
                if let Some(k) = t.sidechain[slot].filter(|&k| k < TRACKS && k != idx && k != MASTER)
                    && lat.gap(at, lat.out(k)) > 0
                {
                    lat.history[k] = true;
                }
                at += fx.latency();
            }
        }
        lat.total = lat.out(MASTER);
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

    /// Retarde l'entrée directe de chaque piste (canaux, clips, écoute, métronome)
    /// de la latence des pistes qui y arrivent.
    fn delay_inputs(&mut self, n: usize) {
        for idx in 0..TRACKS {
            let delay = if self.reduced[idx] { 0 } else { self.lat.gap(self.lat.arrival[idx], 0) };
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

    /// Traite toutes les pistes, une à une dans l'ordre de routage ; le résultat
    /// est dans le master. Chemin de référence du traitement parallèle.
    pub(crate) fn process(&mut self, n: usize, meters: &[PeakMeter]) {
        self.plan_latency();
        self.delay_inputs(n);
        let vca: [f32; TRACKS] = std::array::from_fn(|idx| self.vca_gain(idx));
        let Self { tracks, order, silence, lat, reduced, clock, .. } = self;
        let tracks = Tracks(tracks.as_mut_ptr());
        let cx = Ctx { n, lat, reduced, silence, meters, clock: *clock };
        let mut ready = [false; TRACKS];
        for &idx in order.iter() {
            if idx == MASTER || idx >= TRACKS || ready[idx] {
                continue;
            }
            // SAFETY: un seul thread ; chaque emprunt se termine avant le suivant.
            unsafe { process_track(tracks, idx, &cx, vca[idx], |k| ready[k]) };
            ready[idx] = true;
            // SAFETY: idem.
            let t = unsafe { &*tracks.0.add(idx) };
            let output = if t.output < TRACKS && t.output != idx { t.output } else { MASTER };
            let sends = t.sends;
            // SAFETY: idem ; les liaisons vers soi-même sont écartées.
            unsafe { route(tracks, idx, output, 1.0, &cx) };
            for (target, amount) in sends {
                if amount > 0.0 && target < TRACKS && target != idx {
                    // SAFETY: idem.
                    unsafe { route(tracks, idx, target, amount, &cx) };
                }
            }
        }
        // SAFETY: idem.
        unsafe { process_master(tracks, &cx, |k| ready[k]) };
    }

    /// Plan du traitement parallèle : tâches, liaisons, dépendances. Faux si
    /// l'ordre de routage est périmé (une source traitée après une destination
    /// qui l'a déjà été) : seul le chemin de référence reproduit alors le bloc.
    fn plan_parallel(&mut self) -> bool {
        let vca: [f32; TRACKS] = std::array::from_fn(|idx| self.vca_gain(idx));
        let p = &mut *self.plan;
        p.vca = vca;
        p.pos = [NONE; TRACKS];
        p.pos[MASTER] = TRACKS;
        let mut count = 0;
        for &idx in self.order.iter() {
            if idx == MASTER || idx >= TRACKS || p.pos[idx] != NONE {
                continue;
            }
            p.pos[idx] = count;
            p.task_track[count] = idx;
            count += 1;
        }
        p.graph.reset(count);

        // Liaisons dans l'ordre d'addition du chemin de référence.
        let mut raw = [(0usize, 0usize, 0f32); MAX_ROUTES];
        let mut routes = 0;
        let mut start = [0usize; TRACKS + 1];
        for k in 0..count {
            let src = p.task_track[k];
            let t = &self.tracks[src];
            let output = if t.output < TRACKS && t.output != src { t.output } else { MASTER };
            let sends = t.sends.iter().filter(|&&(dst, amount)| amount > 0.0 && dst < TRACKS && dst != src);
            for (dst, amount) in std::iter::once((output, 1.0)).chain(sends.copied()) {
                if dst != MASTER {
                    match p.pos[dst] {
                        // Jamais traitée : personne n'entend ce qui y arrive.
                        NONE => continue,
                        at if at < k => return false,
                        at => {
                            if !p.graph.edge(k, at) {
                                return false;
                            }
                        }
                    }
                }
                raw[routes] = (src, dst, amount);
                routes += 1;
                start[dst + 1] += 1;
            }
        }
        // Clés d'entrée latérale déjà traitées : lues par la piste qui les écoute.
        for k in 0..count {
            let idx = p.task_track[k];
            for key in self.tracks[idx].sidechain.iter().flatten() {
                if *key < TRACKS && *key != idx && p.pos[*key] < k && !p.graph.edge(p.pos[*key], k) {
                    return false;
                }
            }
        }
        p.graph.finish();
        for d in 0..TRACKS {
            start[d + 1] += start[d];
        }
        p.start = start;
        let mut cursor = start;
        for &(src, dst, amount) in &raw[..routes] {
            p.routes[cursor[dst]] = (src, amount);
            cursor[dst] += 1;
        }
        true
    }

    /// Comme [`Mixer::process`], les pistes indépendantes en parallèle sur
    /// `pool`. Chaque piste additionne elle-même ses entrées, dans l'ordre du
    /// chemin de référence : le résultat est identique au bit près.
    pub(crate) fn process_parallel(&mut self, n: usize, meters: &[PeakMeter], pool: &WorkerPool) {
        if !self.plan_parallel() || self.plan.graph.count() < 2 {
            return self.process(n, meters);
        }
        self.plan_latency();
        self.delay_inputs(n);
        let Self { tracks, silence, lat, reduced, plan, clock, .. } = self;
        let tracks = Tracks(tracks.as_mut_ptr());
        let cx = Ctx { n, lat, reduced, silence, meters, clock: *clock };
        let plan = &**plan;
        let gather = |dst: usize| {
            for &(src, amount) in plan.routes_into(dst) {
                // SAFETY: les sources de `dst` sont terminées (graphe), `dst` est à nous.
                unsafe { route(tracks, src, dst, amount, &cx) };
            }
        };
        pool.run_graph(&plan.graph, &|task| {
            let idx = plan.task_track[task];
            gather(idx);
            // SAFETY: la tâche possède sa piste ; ses clés prêtes sont terminées.
            unsafe { process_track(tracks, idx, &cx, plan.vca[idx], |k| plan.pos[k] < plan.pos[idx]) };
        });
        gather(MASTER);
        // SAFETY: toutes les tâches sont terminées.
        unsafe { process_master(tracks, &cx, |k| plan.pos[k] < TRACKS) };
    }

    /// Temps de calcul (ns) de chaque piste et de chacun de ses inserts depuis
    /// le dernier appel, remis à zéro.
    pub(crate) fn take_cpu(&mut self, mut f: impl FnMut(usize, u64, &[u64; INSERT_SLOTS])) {
        for (idx, t) in self.tracks.iter_mut().enumerate() {
            f(idx, t.cpu_ns, &t.insert_ns);
            t.cpu_ns = 0;
            t.insert_ns = [0; INSERT_SLOTS];
        }
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

    // --- Traitement parallèle ---

    /// Mixer chargé : bus, envois, VCA, clés d'entrée latérale, limiteurs (PDC).
    fn busy_mixer() -> Mixer {
        let mut m = Mixer::new(64, 4_800);
        let fx = |kind: &str| {
            let mut fx = crate::registry::create_effect(kind).unwrap();
            fx.set_sample_rate(SR);
            fx
        };
        for t in 1..TRACKS {
            m.tracks[t].gain = 0.5 + t as f32 * 0.01;
            m.tracks[t].pan = (t as f32 * 0.37).sin();
        }
        for t in 1..9 {
            m.tracks[t].output = 20 + t % 3;
            m.tracks[t].sends[0] = (25, 0.3);
            m.tracks[t].inserts[0] = Some(fx(if t % 2 == 0 { "saturator" } else { "eq" }));
        }
        m.tracks[3].inserts[1] = Some(limiter());
        m.tracks[4].inserts[1] = Some(fx("compressor"));
        m.tracks[4].sidechain[1] = Some(2);
        m.tracks[21].inserts[0] = Some(limiter());
        m.tracks[21].sends[1] = (25, 0.5);
        m.tracks[22].inserts[0] = Some(fx("compressor"));
        m.tracks[22].sidechain[0] = Some(1);
        m.tracks[25].inserts[0] = Some(fx("reverb"));
        m.tracks[10].vca = Some(30);
        m.tracks[30].gain = 0.25;
        m.tracks[MASTER].inserts[0] = Some(fx("compressor"));
        m.tracks[MASTER].sidechain[0] = Some(5);
        reorder(&mut m);
        m
    }

    fn feed(m: &mut Mixer, block: usize, n: usize) {
        m.clear(n);
        for t in 1..TRACKS {
            let (l, r) = m.input(t, n);
            for k in 0..n {
                let x = ((block * n + k) as f32 * 0.013 * t as f32).sin() * 0.3;
                l[k] = x;
                r[k] = -0.5 * x;
            }
        }
    }

    /// Master et pistes traitées identiques au bit près. Une piste absente de
    /// l'ordre n'est jamais traitée ni entendue : son tampon ne compte pas.
    fn assert_same(a: &Mixer, b: &Mixer, n: usize) {
        let heard = (0..TRACKS).filter(|&t| t == MASTER || a.order.contains(&t));
        for (idx, (ta, tb)) in heard.map(|t| (t, (&a.tracks[t], &b.tracks[t]))) {
            let same = |x: &[f32], y: &[f32]| x[..n].iter().zip(&y[..n]).all(|(x, y)| x.to_bits() == y.to_bits());
            assert!(same(&ta.l, &tb.l) && same(&ta.r, &tb.r), "piste {idx}");
        }
    }

    #[test]
    fn parallel_mix_is_bit_identical_to_the_reference() {
        let pool = WorkerPool::new(3);
        let (mut a, mut b) = (busy_mixer(), busy_mixer());
        let meters = meters();
        let mut heard = 0.0f32;
        for block in 0..300 {
            let n = [64, 17, 1, 64][block % 4];
            feed(&mut a, block, n);
            feed(&mut b, block, n);
            a.process(n, &meters);
            b.process_parallel(n, &meters, &pool);
            assert_same(&a, &b, n);
            heard = heard.max(peak(a.master(n).0));
        }
        assert!(heard > 0.01, "le master reçoit du signal : {heard}");
        assert!(a.latency() > 0);
    }

    #[test]
    fn stale_routing_orders_stay_bit_identical() {
        // Ordres périmés (destination avant sa source, pistes absentes, doublons) :
        // repli sur le chemin de référence, aucun blocage.
        let pool = WorkerPool::new(4);
        let (mut a, mut b) = (busy_mixer(), busy_mixer());
        let meters = meters();
        let good = a.order.clone();
        let orders: Vec<Box<[usize]>> = vec![
            good.iter().rev().copied().collect(),
            good.iter().copied().filter(|t| t % 2 == 0).collect(),
            good.iter().chain(good.iter()).copied().collect(),
            [MASTER, 40, 1, 1].into(),
            Box::new([]),
            good.clone(),
        ];
        for block in 0..600 {
            let order = &orders[block % orders.len()];
            a.order = order.clone();
            b.order = order.clone();
            if block % 7 == 0 {
                // Routage changé sans nouvel ordre.
                let t = 1 + block % 8;
                a.tracks[t].output = 20 + block % 5;
                b.tracks[t].output = 20 + block % 5;
            }
            feed(&mut a, block, 64);
            feed(&mut b, block, 64);
            a.process(64, &meters);
            b.process_parallel(64, &meters, &pool);
            assert_same(&a, &b, 64);
        }
    }
}
