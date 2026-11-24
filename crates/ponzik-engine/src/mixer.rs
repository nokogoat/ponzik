//! Mixer comme celui de FL Studio : la piste 0 est le master, chaque piste a
//! ses effets en insert, son volume, son pan, une sortie et des envois. Le
//! nombre de pistes suit le projet ([`DEFAULT_TRACKS`] par défaut, jusqu'à
//! [`MAX_TRACKS`]) : tout ce qui est rangé par piste est dimensionné à la
//! création du mixer. Pour changer de nombre de pistes, l'interface prépare un
//! nouveau mixer ([`TrackRemap`]) que le moteur échange contre l'ancien, sans
//! allouer (voir [`crate::Command::RemapTracks`]).
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

use crate::effect::{ChannelNotes, Effect};
use crate::freeze::{FreezeClock, Frozen};
use crate::input::InputSource;
use crate::meter::{PeakMeter, peak};
use crate::modulation::{MOD_CHUNK, ModClock, ModInput, ModSet};
use crate::pool::{Graph, MAX_TASKS, WorkerPool};
use crate::xfader::Side;

/// Pistes d'un nouveau projet : le master + 32.
pub const DEFAULT_TRACKS: usize = 33;
/// Most tracks a mixer holds: the master + 999.
pub const MAX_TRACKS: usize = 1_000;
pub const MASTER: usize = 0;
pub const INSERT_SLOTS: usize = 8;
pub const SEND_SLOTS: usize = 4;
/// Below this level (about −94 dBFS) a block counts as silence for the smart
/// disable of the inserts ([`Mixer::set_smart_disable`]).
pub const SILENT_LEVEL: f32 = 2e-5;
/// Silence an insert must give back (silent input, silent output) before it
/// sleeps, in seconds, on top of its own [`Effect::tail`].
pub const SLEEP_HOLD_SECONDS: f32 = 0.5;
/// Silence a plugin (CLAP, VST 3) must give back before it sleeps, on top of
/// [`SLEEP_HOLD_SECONDS`]: plugins do not tell their tail.
pub const PLUGIN_TAIL_SECONDS: f32 = 2.0;
/// `Track::quiet` of a sleeping insert.
const ASLEEP: u32 = u32::MAX;

/// Smart disable: inserts fed silence that give silence back stop being
/// processed until sound comes in again (like FL Studio's smart disable).
#[derive(Clone, Copy, Default)]
pub(crate) struct Sleep {
    /// Frames of silence before an insert sleeps; 0: smart disable off.
    hold: u32,
    sample_rate: f32,
}

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
    /// Frozen: this render replaces the track's input and inserts (and its
    /// latency); the fader, pan, sends and routing stay live.
    pub frozen: Option<Box<Frozen>>,
    /// Offline capture after the inserts, before the fader (interleaved stereo).
    pub(crate) tap: Option<Vec<f32>>,
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
    /// Frames each insert has turned silence into silence ([`ASLEEP`]: it sleeps).
    quiet: [u32; INSERT_SLOTS],
    /// The block's output (post-fader) is all zeros: routing it adds nothing.
    silent: bool,
    /// Side of the crossfader the track listens to.
    pub xfade: Side,
    /// Cue: the track is heard on the cue bus (post-fader, before the crossfader).
    pub cue: bool,
    cue_l: Vec<f32>,
    cue_r: Vec<f32>,
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
            frozen: None,
            tap: None,
            l: vec![0.0; max_block],
            r: vec![0.0; max_block],
            input_delay: Delay::new(max_block, max_latency),
            output_delay: Delay::new(max_block, max_latency),
            key_l: vec![0.0; max_block],
            key_r: vec![0.0; max_block],
            cpu_ns: 0,
            insert_ns: [0; INSERT_SLOTS],
            quiet: [0; INSERT_SLOTS],
            silent: false,
            xfade: Side::None,
            cue: false,
            cue_l: vec![0.0; max_block],
            cue_r: vec![0.0; max_block],
        }
    }

    /// True if the insert in `slot` sleeps (smart disable).
    pub fn insert_asleep(&self, slot: usize) -> bool {
        self.quiet.get(slot) == Some(&ASLEEP)
    }

    /// Wakes the insert in `slot` (replaced, or a parameter changed).
    pub(crate) fn wake(&mut self, slot: usize) {
        if let Some(q) = self.quiet.get_mut(slot) {
            *q = 0;
        }
    }

    /// Latence cumulée des inserts actifs ; `reduced` : ceux qui anticipent
    /// sont court-circuités (écoute à faible latence), donc sans latence.
    fn insert_latency(&self, reduced: bool) -> usize {
        if self.frozen.is_some() {
            return 0;
        }
        let active = self.inserts.iter().zip(&self.bypass).filter(|(_, bypass)| !**bypass);
        active.filter_map(|(fx, _)| fx.as_ref()).filter(|fx| !reduced || fx.keep_when_monitoring()).map(|fx| fx.latency()).sum()
    }

    /// Destination de la sortie de la piste `idx` parmi `count` pistes (le
    /// master si elle sort dans elle-même ou hors du mixer).
    fn output_of(&self, idx: usize, count: usize) -> usize {
        if self.output < count && self.output != idx { self.output } else { MASTER }
    }

    /// Pistes qui reçoivent le signal de `idx` : sa sortie et ses envois actifs.
    fn destinations(&self, idx: usize, count: usize) -> impl Iterator<Item = usize> + '_ {
        let sends = self.sends.iter().filter(move |&&(t, amount)| amount > 0.0 && t < count && t != idx);
        std::iter::once(self.output_of(idx, count)).chain(sends.map(|&(t, _)| t))
    }
}

/// Ligne de retard stéréo de la compensation : on écrit un bloc, puis on le
/// relit `d` échantillons dans le passé. L'historique non écrit (ligne au repos)
/// est lu comme du silence.
///
/// La capacité (1 s) est réservée sans être touchée : l'anneau n'en utilise
/// que ce que demande la latence du moment ([`Delay::reserve_ring`]) et ne
/// remplit ses cases qu'en écrivant. Une ligne qui dort, ou qui ne retarde
/// que de quelques millisecondes, ne coûte donc presque pas de mémoire.
struct Delay {
    /// Cases écrites au moins une fois (`len` ≤ `ring`) ; la capacité est réservée.
    l: Vec<f32>,
    r: Vec<f32>,
    /// Longueur de l'anneau, jamais au-delà de la capacité.
    ring: usize,
    /// Prochaine case écrite (≤ `l.len()`).
    pos: usize,
    /// Échantillons récents écrits sans interruption.
    filled: usize,
}

/// Pas d'agrandissement de l'anneau, en échantillons.
const RING_STEP: usize = 4_096;

impl Delay {
    fn new(max_block: usize, max_latency: usize) -> Self {
        let cap = if max_latency == 0 { 0 } else { max_block + max_latency };
        Self { l: Vec::with_capacity(cap), r: Vec::with_capacity(cap), ring: 0, pos: 0, filled: 0 }
    }

    /// Assure un anneau d'au moins `len` échantillons (borné par la capacité).
    /// Il ne fait que grandir ; quand il grandit, l'historique est oublié. Sans
    /// allocation : la capacité est réservée dès la création.
    fn reserve_ring(&mut self, len: usize) {
        let len = len.next_multiple_of(RING_STEP).min(self.l.capacity()).min(self.r.capacity());
        if len > self.ring {
            self.ring = len;
            self.pos = 0;
            self.filled = 0;
        }
    }

    /// Ligne au repos : l'historique est oublié.
    fn forget(&mut self) {
        self.filled = 0;
    }

    fn write(&mut self, l: &[f32], r: &[f32]) {
        let len = self.ring;
        if len == 0 {
            return;
        }
        for (&a, &b) in l.iter().zip(r) {
            if self.pos < self.l.len() {
                self.l[self.pos] = a;
                self.r[self.pos] = b;
            } else {
                // Première écriture de cette case : dans la capacité réservée.
                debug_assert!(self.l.len() < self.l.capacity());
                self.l.push(a);
                self.r.push(b);
            }
            self.pos += 1;
            if self.pos == len {
                self.pos = 0;
            }
        }
        self.filled = (self.filled + l.len()).min(len);
    }

    /// Les `n` derniers échantillons écrits, retardés de `d` (`d + n` ≤ longueur).
    fn read(&self, d: usize, n: usize, mut f: impl FnMut(usize, f32, f32)) {
        let len = self.ring;
        if len == 0 {
            (0..n).for_each(|i| f(i, 0.0, 0.0));
            return;
        }
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
    arrival: Vec<usize>,
    /// Latence des inserts actifs de chaque piste.
    own: Vec<usize>,
    /// La sortie de la piste est relue en retard par au moins une liaison.
    history: Vec<bool>,
    /// Latence totale au master, inserts du master compris.
    total: usize,
}

impl Latency {
    fn new(count: usize, max: usize) -> Self {
        Self { max, arrival: vec![0; count], own: vec![0; count], history: vec![false; count], total: 0 }
    }

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
    /// Nombre de pistes du mixer.
    count: usize,
    lat: &'a Latency,
    reduced: &'a [bool],
    /// A track monitoring a resampled output is cut (see [`InputSource::Resample`]).
    monitor: &'a [Option<InputSource>],
    silence: &'a [f32],
    meters: &'a [PeakMeter],
    clock: ModClock,
    sleep: Sleep,
    notes: &'a [ChannelNotes],
    /// Crossfader gains (start and end of the block) by [`Side::index`].
    xf: [(f32, f32); 3],
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

/// Plan du traitement parallèle, refait à chaque bloc sans allocation ; ses
/// tables sont dimensionnées pour le nombre de pistes du mixer.
struct Plan {
    graph: Graph,
    /// Piste de chaque tâche, dans l'ordre de routage.
    task_track: Vec<usize>,
    /// Position de chaque piste dans l'ordre (`NONE` : absente, le nombre de
    /// pistes : master).
    pos: Vec<usize>,
    /// Liaisons qui arrivent à chaque piste, dans l'ordre où le chemin de
    /// référence les additionne : `routes[start[t]..start[t + 1]]` = (source, niveau).
    start: Vec<usize>,
    routes: Vec<(usize, f32)>,
    /// Tables de travail : liaisons dans l'ordre des sources, curseurs d'écriture.
    raw: Vec<(usize, usize, f32)>,
    cursor: Vec<usize>,
}

impl Plan {
    fn new(count: usize) -> Self {
        // Liaisons d'un bloc au plus : une sortie et des envois par piste.
        let routes = count * (1 + SEND_SLOTS);
        Self {
            graph: Graph::new(),
            task_track: vec![0; count],
            pos: vec![NONE; count],
            start: vec![0; count + 1],
            routes: vec![(0, 0.0); routes],
            raw: vec![(0, 0, 0.0); routes],
            cursor: vec![0; count + 1],
        }
    }

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
    pub(crate) reduced: Vec<bool>,
    /// Écoute de l'entrée en direct, piste par piste.
    pub(crate) monitor: Vec<Option<InputSource>>,
    /// Facteur de VCA de chaque piste pour le bloc en cours.
    vca: Vec<f32>,
    /// Pistes déjà traitées dans le bloc (table de travail).
    ready: Vec<bool>,
    plan: Box<Plan>,
    /// Transport of the block, for the modulators.
    pub(crate) clock: ModClock,
    /// Transport of the block, for the frozen tracks.
    pub(crate) freeze_clock: FreezeClock,
    sleep: Sleep,
    /// Notes held on each channel of the rack this block, for the inserts
    /// that follow a channel ([`Effect::channel_notes`]).
    pub(crate) notes: Vec<ChannelNotes>,
    /// Crossfader gains by [`Side::index`] (see [`crate::xfader::table`]),
    /// and those the last block ended on (ramped from one to the other).
    pub xf_gain: [f32; 3],
    xf_prev: [f32; 3],
    /// Cue bus blended into what the device plays: 0 the master only, 1 the cue only.
    pub cue_mix: f32,
    /// What the device plays when a track is cued (master and cue blended).
    heard_l: Vec<f32>,
    heard_r: Vec<f32>,
    cueing: bool,
}

/// Ordre par défaut de `count` pistes : toutes vont directement au master.
pub fn default_order(count: usize) -> Box<[usize]> {
    (1..count).rev().collect()
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
    // Zeros added without delay change nothing (an empty or silent track).
    if s.silent && delay == 0 {
        return;
    }
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
    let Track { inserts, bypass, sidechain, l, r, key_l, key_r, insert_ns, quiet, .. } = unsafe { &mut *tracks.0.add(idx) };
    // Latence du signal de la piste à l'entrée de l'insert courant.
    let mut at = cx.lat.arrival[idx];
    for slot in 0..INSERT_SLOTS {
        let key = sidechain[slot].filter(|&k| k < cx.count && k != idx);
        let delay = key.map_or(0, |k| cx.lat.gap(at, cx.lat.out(k)));
        if bypass[slot] {
            continue;
        }
        let Some(fx) = inserts[slot].as_mut() else { continue };
        if cx.reduced[idx] && fx.latency() > 0 && !fx.keep_when_monitoring() {
            continue;
        }
        at += fx.latency();
        // Smart disable: a sleeping insert lets silence through untouched.
        let sleepy = cx.sleep.hold > 0 && key.is_none();
        let silent_in = sleepy && is_silent(&l[a..b], &r[a..b]);
        if sleepy && quiet[slot] == ASLEEP {
            if silent_in {
                continue;
            }
            quiet[slot] = 0;
        }
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
        fx.channel_notes(cx.notes);
        fx.process(&mut l[a..b], &mut r[a..b]);
        insert_ns[slot] += started.elapsed().as_nanos() as u64;
        if sleepy {
            quiet[slot] = if silent_in && is_silent(&l[a..b], &r[a..b]) {
                let tail = (fx.tail().max(0.0) * cx.sleep.sample_rate) as u32;
                let q = quiet[slot].saturating_add((b - a) as u32);
                if q >= cx.sleep.hold.saturating_add(tail) { ASLEEP } else { q }
            } else {
                0
            };
        }
    }
}

/// True if both channels stay under [`SILENT_LEVEL`].
fn is_silent(l: &[f32], r: &[f32]) -> bool {
    peak(l).max(peak(r)) < SILENT_LEVEL
}

/// Inserts, fader, crête et historique d'une piste hors master.
///
/// # Safety
/// Comme [`run_inserts`].
unsafe fn process_track(tracks: Tracks, idx: usize, cx: &Ctx, vca: f32, ready: impl Fn(usize) -> bool) {
    let started = Instant::now();
    let n = cx.n;
    // SAFETY: voir la fonction.
    let frozen = unsafe { (*tracks.0.add(idx)).frozen.is_some() };
    if frozen {
        // SAFETY: voir la fonction ; l'emprunt se termine aussitôt.
        let Track { frozen, l, r, .. } = unsafe { &mut *tracks.0.add(idx) };
        if let Some(f) = frozen.as_mut() {
            f.read(&mut l[..n], &mut r[..n]);
        }
    } else {
        // SAFETY: voir la fonction.
        unsafe { run_inserts(tracks, idx, cx, ready) };
    }
    // SAFETY: voir la fonction.
    let t = unsafe { &mut *tracks.0.add(idx) };
    if let Some(tap) = t.tap.as_mut() {
        tap.extend(t.l[..n].iter().zip(&t.r[..n]).flat_map(|(&a, &b)| [a, b]));
    }
    let (gl, gr) = balance_gains(t.pan);
    // Recording a resampled output: the track itself is routed nowhere, so its
    // own sound never comes back into what it records.
    let cut = matches!(cx.monitor.get(idx), Some(Some(InputSource::Resample(_))));
    let (gl, gr) = if t.muted || cut { (0.0, 0.0) } else { (gl * t.gain * vca, gr * t.gain * vca) };
    for i in 0..n {
        t.l[i] *= gl;
        t.r[i] *= gr;
    let (gl, gr) = if t.muted { (0.0, 0.0) } else { (gl * t.gain * vca, gr * t.gain * vca) };
    if t.cue {
        // Cue: post-fader, before the crossfader.
        for i in 0..n {
            t.cue_l[i] = t.l[i] * gl;
            t.cue_r[i] = t.r[i] * gr;
        }
    }
    let (x0, x1) = cx.xf[t.xfade.index()];
    if x0 == x1 {
        let (gl, gr) = (gl * x1, gr * x1);
        for i in 0..n {
            t.l[i] *= gl;
            t.r[i] *= gr;
        }
    } else {
        // A crossfader move: ramped over the block.
        let step = (x1 - x0) / n.max(1) as f32;
        for i in 0..n {
            let x = x0 + step * (i + 1) as f32;
            t.l[i] *= gl * x;
            t.r[i] *= gr * x;
        }
    }
    let (pl, pr) = (peak(&t.l[..n]), peak(&t.r[..n]));
    t.silent = pl == 0.0 && pr == 0.0;
    if let Some(m) = cx.meters.get(idx) {
        m.record(pl, pr);
    }
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
    {
        // SAFETY: voir la fonction ; l'emprunt se termine avant les inserts.
        let m = unsafe { &mut *tracks.0.add(MASTER) };
        if let Some(tap) = m.tap.as_mut() {
            tap.extend(m.l[..n].iter().zip(&m.r[..n]).flat_map(|(&a, &b)| [a, b]));
        }
    }
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
    if let Some(meter) = cx.meters.get(MASTER) {
        meter.record(peak(&m.l[..n]), peak(&m.r[..n]));
    }
    m.cpu_ns += started.elapsed().as_nanos() as u64;
}

/// New track layout, prepared off the audio thread: an empty mixer with the
/// new number of tracks, and where each old track goes. The engine moves the
/// state of the kept tracks into it (effects, delay lines, freeze…), then
/// swaps it with its mixer; the old one goes back to the interface in this
/// same box ([`crate::Garbage::Mixer`]).
pub struct TrackRemap {
    pub(crate) mixer: Mixer,
    /// Old index of each new track (`NONE`: a new, empty track).
    pub(crate) from: Box<[usize]>,
    /// New index of each old track (`NONE`: deleted).
    pub(crate) to: Box<[usize]>,
}

impl TrackRemap {
    /// `from[t]`: the old track that becomes track `t` (`None`: a new one);
    /// the master stays first. `order`: routing order of the new mixer.
    /// Allocates: call it off the audio thread.
    pub fn new(from: &[Option<usize>], order: Box<[usize]>, max_block: usize, max_latency: usize) -> Self {
        let count = from.len().clamp(1, MAX_TRACKS);
        let from: Box<[usize]> = (0..count).map(|t| from.get(t).copied().flatten().unwrap_or(NONE)).collect();
        let old = from.iter().filter(|&&f| f != NONE).map(|&f| f + 1).max().unwrap_or(0);
        let mut to = vec![NONE; old];
        for (new, &was) in from.iter().enumerate() {
            if was != NONE {
                to[was] = new;
            }
        }
        let mut mixer = Mixer::with_tracks(count, max_block, max_latency);
        mixer.order = order;
        Self { mixer, from, to: to.into() }
    }

    /// Number of tracks of the new mixer, master included.
    pub fn len(&self) -> usize {
        self.mixer.tracks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.mixer.tracks.is_empty()
    }

    /// New index of the old track `old` (`None`: deleted).
    pub fn new_index(&self, old: usize) -> Option<usize> {
        self.to.get(old).copied().filter(|&t| t < self.len())
    }
}

impl Mixer {
    /// Mixer de [`DEFAULT_TRACKS`] pistes. `max_latency` : plus grand retard
    /// compensé, en échantillons (0 : pas de compensation).
    pub fn new(max_block: usize, max_latency: usize) -> Self {
        Self::with_tracks(DEFAULT_TRACKS, max_block, max_latency)
    }

    /// Mixer of `count` tracks, master included (at most [`MAX_TRACKS`]).
    pub fn with_tracks(count: usize, max_block: usize, max_latency: usize) -> Self {
        let count = count.clamp(1, MAX_TRACKS);
        Self {
            tracks: (0..count).map(|_| Track::new(max_block, max_latency)).collect(),
            order: default_order(count),
            silence: vec![0.0; max_block],
            lat: Latency::new(count, max_latency),
            reduced: vec![false; count],
            monitor: vec![None; count],
            vca: vec![1.0; count],
            ready: vec![false; count],
            plan: Box::new(Plan::new(count)),
            clock: ModClock::default(),
            freeze_clock: FreezeClock::default(),
            sleep: Sleep::default(),
            notes: vec![ChannelNotes::EMPTY; crate::engine::MAX_CHANNELS],
            xf_gain: [1.0; 3],
            xf_prev: [1.0; 3],
            cue_mix: 1.0,
            heard_l: vec![0.0; max_block],
            heard_r: vec![0.0; max_block],
            cueing: false,
        }
    }

    /// Smart disable of the inserts: on, an insert fed silence that has given
    /// silence back for [`SLEEP_HOLD_SECONDS`] plus its [`Effect::tail`] is no
    /// longer processed (the silence passes through) until sound comes in
    /// again. Inserts keyed by a sidechain never sleep. Off, every insert runs
    /// on every block, as an exact render wants.
    pub fn set_smart_disable(&mut self, on: bool, sample_rate: f32) {
        let hold = if on { (SLEEP_HOLD_SECONDS * sample_rate.max(1.0)) as u32 } else { 0 };
        self.sleep = Sleep { hold: hold.max(u32::from(on)), sample_rate };
        if !on {
            for t in &mut self.tracks {
                t.quiet = [0; INSERT_SLOTS];
            }
        }
    }

    /// Number of tracks, master included.
    pub fn len(&self) -> usize {
        self.tracks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }

    /// Takes over the tracks of `old` as `remap` says (audio thread, no
    /// allocation): each kept track is swapped with the new, empty track at its
    /// new place, and its links (output, sends, keys, VCA) follow the new
    /// numbering; a link to a deleted track falls back to the master or goes.
    pub(crate) fn adopt(&mut self, old: &mut Mixer, from: &[usize], to: &[usize]) {
        let count = self.tracks.len();
        let map = |t: usize| to.get(t).copied().filter(|&x| x < count);
        for (new, &was) in from.iter().enumerate().take(count) {
            if was >= old.tracks.len() {
                continue;
            }
            std::mem::swap(&mut self.tracks[new], &mut old.tracks[was]);
            self.monitor[new] = match old.monitor[was] {
                Some(InputSource::Resample(k)) => map(k).map(InputSource::Resample),
                m => m,
            };
            let t = &mut self.tracks[new];
            t.output = map(t.output).filter(|&o| o != new).unwrap_or(MASTER);
            for s in &mut t.sends {
                *s = match map(s.0) {
                    Some(dst) if dst != new => (dst, s.1),
                    _ => (MASTER, 0.0),
                };
            }
            for k in &mut t.sidechain {
                *k = k.and_then(map).filter(|&x| x != new);
            }
            t.vca = t.vca.and_then(map).filter(|&v| v != new && v != MASTER);
        }
        self.clock = old.clock;
        self.freeze_clock = old.freeze_clock;
        self.sleep = old.sleep;
        self.lat.total = old.lat.total;
        self.xf_gain = old.xf_gain;
        self.xf_prev = old.xf_prev;
        self.cue_mix = old.cue_mix;
    }

    /// Latency of what [`crate::Tap::Track`] captures, as of the last block:
    /// a track's signal after its inserts (the slowest path into it plus its
    /// own inserts), the master's input.
    pub fn track_latency(&self, track: usize) -> usize {
        match track {
            MASTER => self.lat.arrival[MASTER],
            t if t < self.tracks.len() => self.lat.out(t),
            _ => 0,
        }
    }

    /// Frozen tracks follow the transport of the block.
    fn sync_frozen(&mut self) {
        let clock = self.freeze_clock;
        for f in self.tracks.iter_mut().filter_map(|t| t.frozen.as_mut()) {
            f.sync(&clock);
        }
    }

    /// Latency of a track's post-fader output as of the last block (the
    /// master: the whole mix, its inserts included): what resampling it lags.
    pub fn output_latency(&self, track: usize) -> usize {
        match track {
            MASTER => self.lat.total,
            t if t < self.tracks.len() => self.lat.out(t),
            _ => 0,
        }
    }

    /// Post-fader output of a track for the block just mixed (the master's
    /// output for [`MASTER`]).
    pub(crate) fn output(&self, track: usize, n: usize) -> Option<(&[f32], &[f32])> {
        let t = self.tracks.get(track)?;
        Some((&t.l[..n], &t.r[..n]))
    }

    /// Latence totale au master, en échantillons, mesurée au dernier bloc :
    /// retard du chemin le plus lent, inserts du master compris.
    pub fn latency(&self) -> usize {
        self.lat.total
    }

    /// Latences du bloc à venir, dans l'ordre de routage (sources avant destinations).
    pub(crate) fn plan_latency(&mut self) {
        let Self { tracks, order, lat, reduced, ready: seen, silence, .. } = self;
        let count = tracks.len();
        lat.arrival.fill(0);
        lat.own.fill(0);
        lat.history.fill(false);
        seen.fill(false);
        for &idx in order.iter() {
            if idx == MASTER || idx >= count || seen[idx] {
                continue;
            }
            seen[idx] = true;
            let t = &tracks[idx];
            lat.own[idx] = t.insert_latency(reduced[idx]);
            let out = lat.out(idx);
            for dst in t.destinations(idx, count) {
                // A frozen track ignores what comes in: its render is already in time.
                if tracks[dst].frozen.is_none() {
                    lat.arrival[dst] = lat.arrival[dst].max(out);
                }
            }
        }
        lat.own[MASTER] = tracks[MASTER].insert_latency(reduced[MASTER]);
        seen[MASTER] = true;

        // Une sortie n'est historisée que si une liaison la relit en retard.
        for idx in (0..count).filter(|&t| seen[t]) {
            let t = &tracks[idx];
            if idx != MASTER {
                let out = lat.out(idx);
                let late = t.destinations(idx, count).any(|dst| lat.gap(lat.arrival[dst], out) > 0);
                lat.history[idx] |= late;
            }
            let mut at = lat.arrival[idx];
            for slot in 0..INSERT_SLOTS {
                let Some(fx) = t.inserts[slot].as_ref().filter(|_| !t.bypass[slot] && t.frozen.is_none()) else { continue };
                if reduced[idx] && fx.latency() > 0 && !fx.keep_when_monitoring() {
                    continue;
                }
                if let Some(k) = t.sidechain[slot].filter(|&k| k < count && k != idx && k != MASTER)
                    && lat.gap(at, lat.out(k)) > 0
                {
                    lat.history[k] = true;
                }
                at += fx.latency();
            }
        }
        lat.total = lat.out(MASTER);
        // Plus long retard relu : borné par la latence la plus profonde.
        let deepest = (0..count).map(|t| lat.out(t)).max().unwrap_or(0).min(lat.max);
        if deepest > 0 {
            let ring = silence.len() + deepest;
            for t in tracks.iter_mut() {
                t.input_delay.reserve_ring(ring);
                t.output_delay.reserve_ring(ring);
            }
        }
    }

    pub(crate) fn clear(&mut self, n: usize) {
        for t in &mut self.tracks {
            t.l[..n].fill(0.0);
            t.r[..n].fill(0.0);
        }
    }

    /// Entrée d'une piste, pour y ajouter le signal des canaux ; une piste
    /// hors du mixer est le master.
    pub(crate) fn input(&mut self, track: usize, n: usize) -> (&mut [f32], &mut [f32]) {
        let idx = if track < self.tracks.len() { track } else { MASTER };
        let t = &mut self.tracks[idx];
        (&mut t.l[..n], &mut t.r[..n])
    }

    /// Retarde l'entrée directe de chaque piste (canaux, clips, écoute, métronome)
    /// de la latence des pistes qui y arrivent.
    fn delay_inputs(&mut self, n: usize) {
        for idx in 0..self.tracks.len() {
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

    /// VCA factors of the block.
    fn plan_vca(&mut self) {
        for idx in 0..self.tracks.len() {
            let g = self.vca_gain(idx);
            self.vca[idx] = g;
        }
    }

    /// Traite toutes les pistes, une à une dans l'ordre de routage ; le résultat
    /// est dans le master. Chemin de référence du traitement parallèle.
    pub(crate) fn process(&mut self, n: usize, meters: &[PeakMeter]) {
        self.sync_frozen();
        self.plan_latency();
        self.delay_inputs(n);
        self.plan_vca();
        let Self { tracks, order, silence, lat, reduced, monitor, clock, vca, ready, sleep, notes, .. } = self;
        let count = tracks.len();
        let tracks = Tracks(tracks.as_mut_ptr());
        let cx = Ctx { n, count, lat, reduced, monitor, silence, meters, clock: *clock, sleep: *sleep, notes };
        let xf = self.xf_ramps();
        let Self { tracks, order, silence, lat, reduced, clock, vca, ready, sleep, notes, .. } = self;
        let count = tracks.len();
        let tracks = Tracks(tracks.as_mut_ptr());
        let cx = Ctx { n, count, lat, reduced, silence, meters, clock: *clock, sleep: *sleep, notes, xf };
        ready.fill(false);
        for &idx in order.iter() {
            if idx == MASTER || idx >= count || ready[idx] {
                continue;
            }
            // SAFETY: un seul thread ; chaque emprunt se termine avant le suivant.
            unsafe { process_track(tracks, idx, &cx, vca[idx], |k| ready[k]) };
            ready[idx] = true;
            // SAFETY: idem.
            let t = unsafe { &*tracks.0.add(idx) };
            let output = t.output_of(idx, count);
            let sends = t.sends;
            // SAFETY: idem ; les liaisons vers soi-même sont écartées.
            unsafe { route(tracks, idx, output, 1.0, &cx) };
            for (target, amount) in sends {
                if amount > 0.0 && target < count && target != idx {
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
    /// qui l'a déjà été) ou si le graphe ne tient pas dans le pool : seul le
    /// chemin de référence reproduit alors le bloc.
    fn plan_parallel(&mut self) -> bool {
        self.plan_vca();
        let count = self.tracks.len();
        let p = &mut *self.plan;
        p.pos.fill(NONE);
        p.pos[MASTER] = count;
        let mut tasks = 0;
        for &idx in self.order.iter() {
            if idx == MASTER || idx >= count || p.pos[idx] != NONE {
                continue;
            }
            p.pos[idx] = tasks;
            p.task_track[tasks] = idx;
            tasks += 1;
        }
        if tasks > MAX_TASKS {
            return false;
        }
        p.graph.reset(tasks);

        // Liaisons dans l'ordre d'addition du chemin de référence.
        let mut routes = 0;
        p.start.fill(0);
        for k in 0..tasks {
            let src = p.task_track[k];
            let t = &self.tracks[src];
            let sends = t.sends.iter().filter(|&&(dst, amount)| amount > 0.0 && dst < count && dst != src);
            for (dst, amount) in std::iter::once((t.output_of(src, count), 1.0)).chain(sends.copied()) {
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
                p.raw[routes] = (src, dst, amount);
                routes += 1;
                p.start[dst + 1] += 1;
            }
        }
        // Clés d'entrée latérale déjà traitées : lues par la piste qui les écoute.
        for k in 0..tasks {
            let idx = p.task_track[k];
            for key in self.tracks[idx].sidechain.iter().flatten() {
                if *key < count && *key != idx && p.pos[*key] < k && !p.graph.edge(p.pos[*key], k) {
                    return false;
                }
            }
        }
        p.graph.finish();
        for d in 0..count {
            p.start[d + 1] += p.start[d];
        }
        p.cursor.copy_from_slice(&p.start);
        for &(src, dst, amount) in &p.raw[..routes] {
            p.routes[p.cursor[dst]] = (src, amount);
            p.cursor[dst] += 1;
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
        self.sync_frozen();
        self.plan_latency();
        self.delay_inputs(n);
        let Self { tracks, silence, lat, reduced, monitor, plan, clock, vca, sleep, notes, .. } = self;
        let count = tracks.len();
        let tracks = Tracks(tracks.as_mut_ptr());
        let cx = Ctx { n, count, lat, reduced, monitor, silence, meters, clock: *clock, sleep: *sleep, notes };
        let xf = self.xf_ramps();
        let Self { tracks, silence, lat, reduced, plan, clock, vca, sleep, notes, .. } = self;
        let count = tracks.len();
        let tracks = Tracks(tracks.as_mut_ptr());
        let cx = Ctx { n, count, lat, reduced, silence, meters, clock: *clock, sleep: *sleep, notes, xf };
        let plan = &**plan;
        let vca = &**vca;
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
            unsafe { process_track(tracks, idx, &cx, vca[idx], |k| plan.pos[k] < plan.pos[idx]) };
        });
        gather(MASTER);
        // SAFETY: toutes les tâches sont terminées.
        unsafe { process_master(tracks, &cx, |k| plan.pos[k] < count) };
    }

    /// Temps de calcul (ns) de chaque piste et de chacun de ses inserts depuis
    /// le dernier appel, remis à zéro.
    /// The last argument has bit `slot` set for each sleeping insert.
    pub(crate) fn take_cpu(&mut self, mut f: impl FnMut(usize, u64, &[u64; INSERT_SLOTS], u8)) {
        for (idx, t) in self.tracks.iter_mut().enumerate() {
            let asleep = (0..INSERT_SLOTS).filter(|&s| t.quiet[s] == ASLEEP).fold(0u8, |m, s| m | 1 << s);
            f(idx, t.cpu_ns, &t.insert_ns, asleep);
            t.cpu_ns = 0;
            t.insert_ns = [0; INSERT_SLOTS];
        }
    }

    /// Facteur du VCA qui pilote la piste (1 sans VCA, 0 s'il est muet).
    fn vca_gain(&self, idx: usize) -> f32 {
        match self.tracks[idx].vca.filter(|&v| v < self.tracks.len() && v != idx && v != MASTER) {
            Some(v) if self.tracks[v].muted => 0.0,
            Some(v) => self.tracks[v].gain,
            None => 1.0,
        }
    }

    pub fn master(&self, n: usize) -> (&[f32], &[f32]) {
        let m = &self.tracks[MASTER];
        (&m.l[..n], &m.r[..n])
    }

    /// Crossfader gains of the block: from where the last block ended to the target.
    fn xf_ramps(&self) -> [(f32, f32); 3] {
        std::array::from_fn(|k| (self.xf_prev[k], self.xf_gain[k]))
    }

    /// Ends the block: the crossfader ramps are done, and the cue bus (the
    /// cued tracks summed) is blended with the master into what the device plays.
    pub(crate) fn end_block(&mut self, n: usize) {
        self.xf_prev = self.xf_gain;
        let count = self.tracks.len();
        self.cueing = self.tracks[1.min(count)..].iter().any(|t| t.cue);
        if !self.cueing {
            return;
        }
        let k = self.cue_mix.clamp(0.0, 1.0);
        let (hl, hr) = (&mut self.heard_l[..n], &mut self.heard_r[..n]);
        {
            let m = &self.tracks[MASTER];
            for i in 0..n {
                hl[i] = m.l[i] * (1.0 - k);
                hr[i] = m.r[i] * (1.0 - k);
            }
        }
        for t in self.tracks[1..].iter().filter(|t| t.cue) {
            for i in 0..n {
                hl[i] += t.cue_l[i] * k;
                hr[i] += t.cue_r[i] * k;
            }
        }
        for i in 0..n {
            hl[i] = if hl[i].is_finite() { hl[i].clamp(-1.0, 1.0) } else { 0.0 };
            hr[i] = if hr[i].is_finite() { hr[i].clamp(-1.0, 1.0) } else { 0.0 };
        }
    }

    /// What the device plays: the master, or the master and the cue bus
    /// blended while a track is cued (the master itself stays the full mix).
    pub fn heard(&self, n: usize) -> (&[f32], &[f32]) {
        if self.cueing { (&self.heard_l[..n], &self.heard_r[..n]) } else { self.master(n) }
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
    fn crossfader_and_cue() {
        use crate::xfader::{Curve, Side, table};
        let mut m = Mixer::new(8, 1_024);
        let meters: Vec<PeakMeter> = (0..DEFAULT_TRACKS).map(|_| PeakMeter::default()).collect();
        m.tracks[1].xfade = Side::A;
        m.tracks[2].xfade = Side::B;
        let feed = |m: &mut Mixer| {
            m.clear(8);
            m.input(1, 8).0.fill(0.2);
            m.input(2, 8).0.fill(0.4);
            m.input(3, 8).0.fill(0.1);
        };
        // All the way to A: B is silent, the unassigned track plays on.
        m.xf_gain = table(0.0, Curve::Power);
        feed(&mut m);
        m.process(8, &meters);
        m.end_block(8);
        // First block ramps from unity (no move before) to the new gains.
        assert!((m.master(8).0[7] - 0.3).abs() < 1e-6, "{}", m.master(8).0[7]);
        feed(&mut m);
        m.process(8, &meters);
        m.end_block(8);
        assert!(m.master(8).0.iter().all(|&x| (x - 0.3).abs() < 1e-6));
        // To B, ramped over the block: no jump on the first sample.
        m.xf_gain = table(1.0, Curve::Power);
        feed(&mut m);
        m.process(8, &meters);
        m.end_block(8);
        let l = m.master(8).0;
        assert!(l.windows(2).all(|w| w[1] >= w[0]), "{l:?}");
        assert!((l[0] - (0.1 + 0.2 * 7.0 / 8.0 + 0.4 / 8.0)).abs() < 1e-5, "{}", l[0]);
        assert!((l[7] - 0.5).abs() < 1e-6);
        assert_eq!(m.heard(8).0, m.master(8).0, "no cue: the device plays the master");

        // Cue on track 1, faded out by the crossfader: heard, never in the master.
        m.tracks[1].cue = true;
        m.cue_mix = 1.0;
        feed(&mut m);
        m.process(8, &meters);
        m.end_block(8);
        assert!(m.master(8).0.iter().all(|&x| (x - 0.5).abs() < 1e-6), "the master keeps the full mix");
        assert!(m.heard(8).0.iter().all(|&x| (x - 0.2).abs() < 1e-6), "{:?}", m.heard(8).0);
        m.cue_mix = 0.5;
        feed(&mut m);
        m.process(8, &meters);
        m.end_block(8);
        assert!(m.heard(8).0.iter().all(|&x| (x - 0.35).abs() < 1e-6));
        m.tracks[1].cue = false;
        feed(&mut m);
        m.process(8, &meters);
        m.end_block(8);
        assert_eq!(m.heard(8).0, m.master(8).0);
    }

    #[test]
    fn routing_sends_and_master() {
        let mut m = Mixer::new(8, 1_024);
        let meters: Vec<PeakMeter> = (0..DEFAULT_TRACKS).map(|_| PeakMeter::default()).collect();
        m.tracks[1].output = 2;
        m.tracks[1].sends[0] = (3, 0.5);
        m.tracks[2].gain = 0.5;
        let mut edges = vec![Vec::new(); DEFAULT_TRACKS];
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
        let meters: Vec<PeakMeter> = (0..DEFAULT_TRACKS).map(|_| PeakMeter::default()).collect();
        let mut fx = crate::registry::create_effect("compressor").unwrap();
        fx.set_param(0, -30.0);
        fx.set_param(1, 20.0);
        fx.set_param(2, 0.0);
        m.tracks[1].inserts[0] = Some(fx);
        m.tracks[1].sidechain[0] = Some(2);
        let mut edges = vec![Vec::new(); DEFAULT_TRACKS];
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
        (0..DEFAULT_TRACKS).map(|_| PeakMeter::default()).collect()
    }

    fn limiter() -> Box<dyn Effect> {
        let mut fx = crate::registry::create_effect("limiter").unwrap();
        fx.set_sample_rate(SR);
        fx
    }

    /// Ordre de routage déduit des sorties, envois et clés des pistes.
    fn reorder(m: &mut Mixer) {
        let count = m.len();
        let mut edges = vec![Vec::new(); count];
        for (idx, t) in m.tracks.iter().enumerate().skip(1) {
            edges[idx].extend(t.destinations(idx, count));
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

    /// Counts its blocks; passes the signal through, plus `hum` (a constant:
    /// an effect that makes sound out of silence), and says it has `tail` seconds.
    struct Counter {
        calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        hum: f32,
        tail: f32,
    }

    impl crate::param::Params for Counter {}

    impl Effect for Counter {
        fn kind(&self) -> &'static str {
            "counter"
        }

        fn set_sample_rate(&mut self, _: f32) {}

        fn tail(&self) -> f32 {
            self.tail
        }

        fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            l.iter_mut().chain(r.iter_mut()).for_each(|s| *s += self.hum);
        }
    }

    /// A mixer with a [`Counter`] on track 1, smart disable on (or not), and the counter's calls.
    fn counting(smart: bool, hum: f32, tail: f32) -> (Mixer, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut m = Mixer::new(64, 4_800);
        m.tracks[1].inserts[0] = Some(Box::new(Counter { calls: calls.clone(), hum, tail }));
        m.set_smart_disable(smart, SR);
        (m, calls)
    }

    /// `blocks` blocks of 64 frames, track 1 fed `level`; the master's last left sample.
    fn run(m: &mut Mixer, blocks: usize, level: f32) -> f32 {
        let meters = meters();
        for _ in 0..blocks {
            m.clear(64);
            m.input(1, 64).0.fill(level);
            m.process(64, &meters);
        }
        m.master(64).0[63]
    }

    #[test]
    fn silent_inserts_sleep_then_wake_on_sound() {
        use std::sync::atomic::Ordering::Relaxed;
        let hold = (SLEEP_HOLD_SECONDS * SR) as usize / 64;
        let (mut m, calls) = counting(true, 0.0, 0.0);
        run(&mut m, hold + 10, 0.0);
        assert!(m.tracks[1].insert_asleep(0), "asleep after the hold");
        let before = calls.load(Relaxed);
        assert!((hold..=hold + 1).contains(&before), "processed until it slept: {before}");
        run(&mut m, 500, 0.0);
        assert_eq!(calls.load(Relaxed), before, "no processing while asleep");
        // Sound comes in: the very block is processed, and passes through.
        let out = run(&mut m, 1, 0.5);
        assert_eq!(calls.load(Relaxed), before + 1);
        assert!(!m.tracks[1].insert_asleep(0));
        assert!(out > 0.3, "{out}");
        // A sound under the silence level does not wake it.
        run(&mut m, hold + 10, 0.0);
        let asleep = calls.load(Relaxed);
        run(&mut m, 50, SILENT_LEVEL * 0.5);
        assert_eq!(calls.load(Relaxed), asleep);
        // Replaced, the insert wakes and runs again.
        m.tracks[1].wake(0);
        run(&mut m, 1, 0.0);
        assert_eq!(calls.load(Relaxed), asleep + 1);
    }

    #[test]
    fn smart_disable_respects_tails_generators_keys_and_off() {
        use std::sync::atomic::Ordering::Relaxed;
        let hold = (SLEEP_HOLD_SECONDS * SR) as usize / 64;
        // A delay of 1 s: silent between two echoes, it waits that much longer.
        let (mut m, _) = counting(true, 0.0, 1.0);
        run(&mut m, hold + 10, 0.0);
        assert!(!m.tracks[1].insert_asleep(0), "the echo may still come");
        run(&mut m, (SR as usize) / 64, 0.0);
        assert!(m.tracks[1].insert_asleep(0));
        // Sound out of silence (noise, hum): never asleep.
        let (mut m, calls) = counting(true, 0.01, 0.0);
        run(&mut m, 3 * hold, 0.0);
        assert!(!m.tracks[1].insert_asleep(0));
        assert_eq!(calls.load(Relaxed), 3 * hold);
        // Keyed by a sidechain: never asleep.
        let (mut m, _) = counting(true, 0.0, 0.0);
        m.tracks[1].sidechain[0] = Some(2);
        reorder(&mut m);
        run(&mut m, 3 * hold, 0.0);
        assert!(!m.tracks[1].insert_asleep(0));
        // Off (exact renders): every block runs.
        let (mut m, calls) = counting(false, 0.0, 0.0);
        run(&mut m, 3 * hold, 0.0);
        assert_eq!(calls.load(Relaxed), 3 * hold);
        // Turned off while asleep: wakes.
        let (mut m, calls) = counting(true, 0.0, 0.0);
        run(&mut m, 2 * hold, 0.0);
        let n = calls.load(Relaxed);
        m.set_smart_disable(false, SR);
        run(&mut m, 1, 0.0);
        assert_eq!(calls.load(Relaxed), n + 1);
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
        for t in 1..DEFAULT_TRACKS {
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
        for t in 1..m.len() {
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
        let heard = (0..a.len()).filter(|&t| t == MASTER || a.order.contains(&t));
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
    fn two_hundred_tracks_mix_bit_identical_in_parallel() {
        let build = || {
            let mut m = Mixer::with_tracks(200, 64, 4_800);
            for t in 1..200 {
                m.tracks[t].gain = 0.3 + (t % 13) as f32 * 0.02;
                m.tracks[t].pan = (t as f32 * 0.21).cos();
            }
            // 150 tracks into 40 buses, which go to a group bus; sends to a reverb.
            for t in 1..150 {
                m.tracks[t].output = 150 + t % 40;
                m.tracks[t].sends[0] = (199, 0.2);
            }
            for b in 150..190 {
                m.tracks[b].output = 190 + b % 5;
            }
            for t in (1..199).step_by(9) {
                m.tracks[t].inserts[0] = Some(if t % 2 == 0 { limiter() } else { crate::registry::create_effect("saturator").unwrap() });
            }
            m.tracks[199].inserts[0] = Some(crate::registry::create_effect("reverb").unwrap());
            m.tracks[170].inserts[1] = Some(crate::registry::create_effect("compressor").unwrap());
            m.tracks[170].sidechain[1] = Some(3);
            m.tracks[42].vca = Some(195);
            m.tracks[195].gain = 0.5;
            reorder(&mut m);
            m
        };
        let pool = WorkerPool::new(3);
        let (mut a, mut b) = (build(), build());
        let meters = meters();
        let mut heard = 0.0f32;
        for block in 0..120 {
            let n = [64, 31, 64][block % 3];
            feed(&mut a, block, n);
            feed(&mut b, block, n);
            a.process(n, &meters);
            b.process_parallel(n, &meters, &pool);
            assert_same(&a, &b, n);
            heard = heard.max(peak(a.master(n).0));
        }
        assert!(b.plan_parallel() && b.plan.graph.count() == 199, "the 199 tracks run as a graph");
        assert!(heard > 0.01 && a.latency() > 0);
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
