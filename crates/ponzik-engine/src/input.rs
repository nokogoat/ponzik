//! Entrée audio et enregistrement.
//!
//! L'entrée arrive soit directement dans l'appel du moteur (duplex intégral,
//! [`crate::Engine::process_duplex`]), soit par une file SPSC remplie par un
//! thread de capture ([`input_channel`]). Elle passe par la tranche d'entrée
//! ([`InputStrip`] : gain, coupe-bas, porte douce), puis va à l'écoute des
//! pistes armées et aux prises ([`RecordBuffer`]), toujours sèches : les
//! effets des pistes ne sont jamais enregistrés.
//!
//! Le reste du module est de l'arithmétique pure, testée à part : découpe d'un
//! enregistrement en prises alignées sur le morceau ([`take_slices`]) et mesure
//! de l'aller-retour par une boucle sortie → entrée ([`measure_round_trip`]).

use ponzik_dsp::biquad::{Biquad, BiquadCoefs};
use ponzik_dsp::util::{db_to_gain, decay_coef};

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
            if self.ring.push(stereo(frame)).is_err() {
                break;
            }
            pushed += 1;
        }
        pushed
    }
}

/// Trame entrelacée → stéréo : mono dupliqué, canaux en trop ignorés.
fn stereo(frame: &[f32]) -> [f32; 2] {
    match frame {
        [m] => [*m; 2],
        [l, r, ..] => [*l, *r],
        [] => [0.0; 2],
    }
}

/// Désentrelace jusqu'à `left.len()` trames de `data` ; complète en silence.
pub(crate) fn deinterleave(data: &[f32], channels: usize, left: &mut [f32], right: &mut [f32]) {
    let n = left.len().min(right.len());
    let mut got = 0;
    if channels > 0 {
        for (k, frame) in data.chunks_exact(channels).take(n).enumerate() {
            [left[k], right[k]] = stereo(frame);
            got = k + 1;
        }
    }
    left[got..n].fill(0.0);
    right[got..n].fill(0.0);
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

/// Ce qu'une piste écoute ou enregistre : les deux canaux de l'entrée, ou l'un
/// d'eux en mono (micro sur l'entrée 1, guitare sur l'entrée 2…).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InputSource {
    #[default]
    Stereo,
    /// Entrée 1, en mono.
    Left,
    /// Entrée 2, en mono.
    Right,
}

impl InputSource {
    pub fn is_mono(self) -> bool {
        self != InputSource::Stereo
    }

    /// Les canaux (gauche, droite) que la source donne à une piste stéréo.
    pub(crate) fn pick<'a>(self, l: &'a [f32], r: &'a [f32]) -> (&'a [f32], &'a [f32]) {
        match self {
            InputSource::Stereo => (l, r),
            InputSource::Left => (l, l),
            InputSource::Right => (r, r),
        }
    }
}

/// Coupure du coupe-bas de l'entrée (grondements, chocs sur le pied de micro).
pub const HIGH_PASS_HZ: f32 = 80.0;
/// Atténuation maximale de la porte : douce, elle baisse le bruit sans le couper net.
pub const GATE_RANGE_DB: f32 = -18.0;

/// Réglages de la tranche d'entrée, appliqués avant l'écoute et l'enregistrement.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InputStrip {
    /// Gain numérique, en dB (−24 à +24).
    pub gain_db: f32,
    /// Coupe-bas à [`HIGH_PASS_HZ`], 12 dB/octave.
    pub high_pass: bool,
    /// Porte de bruit douce (atténuation de [`GATE_RANGE_DB`] sous le seuil).
    pub gate: bool,
    /// Seuil d'ouverture de la porte, en dBFS.
    pub gate_threshold_db: f32,
}

impl Default for InputStrip {
    fn default() -> Self {
        Self { gain_db: 0.0, high_pass: false, gate: false, gate_threshold_db: -50.0 }
    }
}

/// Traitement de la tranche d'entrée, côté moteur (sans allocation).
pub(crate) struct InputChain {
    pub(crate) settings: InputStrip,
    sample_rate: f32,
    /// Gain appliqué, lissé vers la consigne (pas de clic quand on tourne le bouton).
    gain: f32,
    hp: [Biquad; 2],
    /// Enveloppe du détecteur de la porte.
    env: f32,
    /// Gain courant de la porte (1 : ouverte).
    gate_gain: f32,
    /// Échantillons restants avant de pouvoir refermer la porte.
    hold: u32,
}

impl InputChain {
    pub(crate) fn new(sample_rate: f32) -> Self {
        let mut chain = Self {
            settings: InputStrip::default(),
            sample_rate,
            gain: 1.0,
            hp: [Biquad::default(), Biquad::default()],
            env: 0.0,
            gate_gain: 1.0,
            hold: 0,
        };
        chain.set(InputStrip::default());
        chain
    }

    pub(crate) fn set(&mut self, settings: InputStrip) {
        if settings.high_pass && !self.settings.high_pass {
            self.hp.iter_mut().for_each(Biquad::reset);
        }
        self.settings = settings;
        let coefs = BiquadCoefs::highpass(HIGH_PASS_HZ, std::f32::consts::FRAC_1_SQRT_2, self.sample_rate);
        self.hp.iter_mut().for_each(|b| b.coefs = coefs);
    }

    /// Gain courant de la porte (1 : ouverte), pour l'affichage.
    pub(crate) fn gate_gain(&self) -> f32 {
        if self.settings.gate { self.gate_gain } else { 1.0 }
    }

    /// Traite un bloc ; renvoie vrai si l'entrée sature (convertisseur à fond,
    /// ou gain qui fait dépasser 0 dBFS).
    pub(crate) fn process(&mut self, l: &mut [f32], r: &mut [f32]) -> bool {
        let s = self.settings;
        let target = db_to_gain(s.gain_db.clamp(-24.0, 24.0));
        let smooth = 1.0 - decay_coef(0.01, self.sample_rate);
        let (attack, release) = (decay_coef(0.001, self.sample_rate), decay_coef(0.08, self.sample_rate));
        let (open_speed, close_speed) = (1.0 - decay_coef(0.002, self.sample_rate), 1.0 - decay_coef(0.12, self.sample_rate));
        let open_at = db_to_gain(s.gate_threshold_db);
        // Hystérésis : la porte se referme 4 dB sous le seuil d'ouverture.
        let close_at = open_at * 0.63;
        let floor = db_to_gain(GATE_RANGE_DB);
        let hold = (0.05 * self.sample_rate) as u32;
        let mut clipped = false;
        for (a, b) in l.iter_mut().zip(r.iter_mut()) {
            clipped |= a.abs() >= 0.999 || b.abs() >= 0.999;
            self.gain += (target - self.gain) * smooth;
            let (mut x, mut y) = (*a * self.gain, *b * self.gain);
            if s.high_pass {
                x = self.hp[0].process(x);
                y = self.hp[1].process(y);
            }
            if s.gate {
                let level = x.abs().max(y.abs());
                let coef = if level > self.env { attack } else { release };
                self.env = level + (self.env - level) * coef;
                let want = if self.env >= open_at {
                    self.hold = hold;
                    1.0
                } else if self.env >= close_at || self.hold > 0 {
                    self.hold = self.hold.saturating_sub(1);
                    self.gate_gain.max(floor)
                } else {
                    floor
                };
                let speed = if want > self.gate_gain { open_speed } else { close_speed };
                self.gate_gain += (want - self.gate_gain) * speed;
                x *= self.gate_gain;
                y *= self.gate_gain;
            }
            clipped |= x.abs() >= 1.0 || y.abs() >= 1.0;
            *a = x;
            *b = y;
        }
        clipped
    }
}

/// Sauts de transport gardés par une prise (boucles, déplacements).
pub const MAX_JUMPS: usize = 1_024;

/// Tampon d'enregistrement préalloué par l'interface, rempli par le moteur
/// puis rendu par [`crate::EngineHandle::take_recordings`].
///
/// La capacité est la longueur de `left` ; `len` trames sont écrites. Un tampon
/// mono ([`RecordBuffer::mono`]) a un `right` vide.
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
    /// Ce que la prise enregistre ; une source mono remplit `left` seul.
    pub source: InputSource,
    /// Latence de la compensation du mixer (trames) à la première trame :
    /// ce qu'on entendait avait été calculé autant de trames plus tôt.
    pub mix_latency: usize,
    /// Échantillons par tick au début de la prise.
    pub samples_per_tick: f64,
    /// Sauts du transport pendant la prise : (trame du tampon, tick où l'on
    /// reprend). Préalloué ; les sauts au-delà de la capacité sont ignorés.
    pub jumps: Vec<(usize, f64)>,
    /// Morceaux ajoutés pendant la prise ([`crate::Command::RecordChunk`]) :
    /// la prise continue dedans quand `left`/`right` sont pleins. La place des
    /// pointeurs est préallouée ([`MAX_RECORD_CHUNKS`]) ; [`RecordBuffer::flatten`]
    /// les recolle à la suite, hors du thread audio.
    pub chunks: Vec<Box<RecordChunk>>,
}

/// Morceaux qu'une prise peut recevoir en plus de son tampon de départ.
pub const MAX_RECORD_CHUNKS: usize = 128;

/// Suite d'un tampon d'enregistrement, allouée (et écrite) par l'interface
/// quand la prise s'allonge : `right` est vide pour une prise mono.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RecordChunk {
    pub left: Vec<f32>,
    pub right: Vec<f32>,
}

impl RecordChunk {
    /// `frames` trames, mono ou stéréo, mémoire déjà écrite (pas de défaut de
    /// page sur le thread audio).
    pub fn new(frames: usize, mono: bool) -> Self {
        Self { left: zeroed(frames), right: if mono { Vec::new() } else { zeroed(frames) } }
    }

    pub fn frames(&self) -> usize {
        if self.right.is_empty() { self.left.len() } else { self.left.len().min(self.right.len()) }
    }

    pub fn is_mono(&self) -> bool {
        self.right.is_empty()
    }
}

impl RecordBuffer {
    /// Alloue `frames` trames stéréo. La mémoire est écrite tout de suite (pas
    /// de `calloc` paresseux) : aucun défaut de page sur le thread audio.
    pub fn new(frames: usize, sample_rate: f32) -> Self {
        let mut buf = Self::mono(frames, sample_rate);
        buf.right = zeroed(frames);
        buf.source = InputSource::Stereo;
        buf
    }

    /// Tampon mono de `frames` trames, pour une source [`InputSource::Left`] ou
    /// [`InputSource::Right`] : moitié moins de mémoire.
    pub fn mono(frames: usize, sample_rate: f32) -> Self {
        Self {
            left: zeroed(frames),
            right: Vec::new(),
            len: 0,
            start_tick: 0.0,
            sample_rate,
            track: 0,
            source: InputSource::Left,
            mix_latency: 0,
            samples_per_tick: 0.0,
            jumps: Vec::with_capacity(MAX_JUMPS),
            chunks: Vec::with_capacity(MAX_RECORD_CHUNKS),
        }
    }

    /// Tampon de `seconds` secondes (par exemple 600 pour 10 minutes).
    pub fn with_duration(seconds: f32, sample_rate: f32) -> Self {
        Self::new((seconds.max(0.0) * sample_rate) as usize, sample_rate)
    }

    /// Tampon de `seconds` secondes pour `source` (mono si elle l'est).
    pub fn for_source(source: InputSource, seconds: f32, sample_rate: f32) -> Self {
        let frames = (seconds.max(0.0) * sample_rate) as usize;
        let mut buf = if source.is_mono() { Self::mono(frames, sample_rate) } else { Self::new(frames, sample_rate) };
        buf.source = source;
        buf
    }

    pub fn is_mono(&self) -> bool {
        self.right.is_empty()
    }

    /// Trames du tampon de départ (sans les morceaux ajoutés).
    fn base(&self) -> usize {
        if self.is_mono() { self.left.len() } else { self.left.len().min(self.right.len()) }
    }

    /// Trames que la prise peut tenir, morceaux ajoutés compris.
    pub fn capacity(&self) -> usize {
        self.base() + self.chunks.iter().map(|c| c.frames()).sum::<usize>()
    }

    pub fn is_full(&self) -> bool {
        self.len >= self.capacity()
    }

    /// Trames encore libres.
    pub fn room(&self) -> usize {
        self.capacity().saturating_sub(self.len)
    }

    /// Ajoute ce qui tient ; renvoie le nombre de trames écrites. Un tampon
    /// mono ne garde que `left`.
    pub fn write(&mut self, left: &[f32], right: &[f32]) -> usize {
        let n = left.len().min(right.len()).min(self.room());
        let mono = self.is_mono();
        let mut done = 0;
        // Position dans le tampon de départ, puis dans chaque morceau.
        let mut at = self.len;
        let base = self.base();
        let parts = std::iter::once((&mut self.left[..base], &mut self.right[..if mono { 0 } else { base }]))
            .chain(self.chunks.iter_mut().map(|c| {
                let k = c.frames();
                let right = if c.right.is_empty() { &mut c.right[..] } else { &mut c.right[..k] };
                (&mut c.left[..k], right)
            }));
        for (dl, dr) in parts {
            if done == n {
                break;
            }
            if at >= dl.len() {
                at -= dl.len();
                continue;
            }
            let k = (dl.len() - at).min(n - done);
            dl[at..at + k].copy_from_slice(&left[done..done + k]);
            if !mono && !dr.is_empty() {
                dr[at..at + k].copy_from_slice(&right[done..done + k]);
            }
            done += k;
            at = 0;
        }
        self.len += done;
        done
    }

    /// Recolle les morceaux ajoutés au tampon de départ : `left[..len]` (et
    /// `right[..len]`) tiennent alors toute la prise. Alloue : hors du thread audio.
    pub fn flatten(&mut self) {
        let base = self.base();
        let mono = self.is_mono();
        self.left.truncate(base);
        self.right.truncate(if mono { 0 } else { base });
        for c in self.chunks.drain(..) {
            let k = c.frames();
            self.left.extend_from_slice(&c.left[..k]);
            if !mono {
                self.right.extend_from_slice(&c.right[..k]);
            }
        }
    }

    /// Ramène le tampon à `frames` trames, sans morceaux ajoutés, pour une
    /// prochaine prise (la mémoire au-delà est rendue).
    pub fn shrink_for_reuse(&mut self, frames: usize) {
        self.chunks.clear();
        self.left.truncate(frames);
        self.left.shrink_to_fit();
        if !self.is_mono() {
            self.right.truncate(frames);
            self.right.shrink_to_fit();
        }
        self.len = 0;
    }

    /// Note un saut du transport à la trame courante (sans allocation).
    pub(crate) fn jump(&mut self, tick: f64) {
        if self.jumps.len() < self.jumps.capacity() {
            self.jumps.push((self.len, tick));
        }
    }

    /// Échantillon des trames écrites (le surplus préalloué est libéré) ; le
    /// mono est recopié à droite, comme un fichier mono chargé.
    pub fn into_sample(mut self, name: impl Into<String>) -> Sample {
        self.flatten();
        self.left.truncate(self.len);
        self.left.shrink_to_fit();
        if self.is_mono() {
            self.right = self.left.clone();
        } else {
            self.right.truncate(self.len);
            self.right.shrink_to_fit();
        }
        Sample { name: name.into(), sample_rate: self.sample_rate, left: self.left, right: self.right, markers: Vec::new() }
    }
}

fn zeroed(frames: usize) -> Vec<f32> {
    let mut v = Vec::with_capacity(frames);
    v.resize(frames, 0.0);
    v
}

/// Morceau d'un enregistrement posé dans le morceau : trames `[from, to)` du
/// tampon, la première au tick `tick`. `pass` : numéro du passage de boucle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TakeSlice {
    pub from: usize,
    pub to: usize,
    pub tick: f64,
    pub pass: usize,
}

/// Découpe un enregistrement en prises alignées sur le morceau.
///
/// Le moteur a écrit `len` trames à partir du tick `start_tick` ; `jumps`
/// liste les sauts du transport (trame, tick de reprise), typiquement à chaque
/// tour de boucle. `shift` est le retard total, en trames, entre l'instant où
/// une position du morceau a été calculée et celui où la réponse du musicien
/// est arrivée dans le tampon : compensation du mixer + sortie + entrée +
/// réglage manuel. La trame `j` du tampon appartient donc au morceau à la
/// trame `j − shift` du moteur, et chaque passage commence `shift` trames
/// après le saut qui l'ouvre.
///
/// `punch` (début, fin en ticks) ne garde que ce qui tombe dedans. Les
/// morceaux de moins de `min_frames` trames sont écartés.
pub fn take_slices(
    len: usize,
    start_tick: f64,
    jumps: &[(usize, f64)],
    shift: usize,
    samples_per_tick: f64,
    punch: Option<(f64, f64)>,
    min_frames: usize,
) -> Vec<TakeSlice> {
    let spt = samples_per_tick.max(1e-9);
    let mut bounds: Vec<(usize, f64)> = vec![(0, start_tick)];
    bounds.extend(jumps.iter().copied().filter(|&(at, _)| at > 0 && at < len));
    let mut out = Vec::new();
    for (pass, k) in (0..bounds.len()).enumerate() {
        let (at, tick) = bounds[k];
        // Le premier passage commence avant sa position (le tampon débute `shift` trames trop tard).
        let (mut from, mut tick) = if k == 0 { (0, tick - shift as f64 / spt) } else { (at + shift, tick) };
        let mut to = bounds.get(k + 1).map_or(len, |&(next, _)| next + shift).min(len);
        if tick < 0.0 {
            from += (-tick * spt).round() as usize;
            tick = 0.0;
        }
        if let Some((a, b)) = punch {
            let end = tick + to.saturating_sub(from) as f64 / spt;
            let (lo, hi) = (tick.max(a), end.min(b));
            if hi <= lo {
                continue;
            }
            let skip = ((lo - tick) * spt).round() as usize;
            to = (from + ((hi - tick) * spt).round() as usize).min(to);
            from += skip;
            tick = lo;
        }
        if to > from && to - from >= min_frames.max(1) {
            out.push(TakeSlice { from, to, tick, pass });
        }
    }
    out
}

/// Où poser un morceau de prise : tick entier du clip, et décalage dans le
/// fichier en secondes, pour que la trame `from` tombe pile sur `tick`.
pub fn clip_placement(slice: &TakeSlice, samples_per_tick: f64, sample_rate: f64) -> (u32, f64) {
    let start = slice.tick.max(0.0).ceil();
    let extra = (start - slice.tick) * samples_per_tick;
    (start as u32, (slice.from as f64 + extra) / sample_rate.max(1.0))
}

/// Instants des clics de calibrage, en secondes depuis son début.
pub const CALIBRATION_CLICKS: [f32; 4] = [0.25, 0.75, 1.25, 1.75];
/// Durée enregistrée pendant le calibrage, en secondes.
pub const CALIBRATION_SECONDS: f32 = 2.25;
/// Plus grand aller-retour mesurable, en secondes.
const CALIBRATION_MAX_LAG: f32 = 0.45;

/// Échantillon `k` du clic de calibrage : 2 ms de sinus à 3 kHz sous une
/// fenêtre de Hann, assez bref pour se repérer à l'échantillon près.
pub fn calibration_click(k: usize, sample_rate: f32) -> f32 {
    let len = (0.002 * sample_rate).round().max(8.0) as usize;
    if k >= len {
        return 0.0;
    }
    let t = k as f32 / sample_rate;
    let w = 0.5 - 0.5 * (std::f32::consts::TAU * k as f32 / len as f32).cos();
    0.5 * w * (std::f32::consts::TAU * 3_000.0 * t).sin()
}

/// Mesure l'aller-retour sortie → entrée à partir d'un enregistrement qui
/// commence avec les clics de [`CALIBRATION_CLICKS`]. Pour chaque clic, la
/// corrélation avec le clic émis repère son retour ; le résultat est la
/// médiane des retards, ou `None` si les clics ne reviennent pas nettement
/// (rien branché, trop de bruit, retards incohérents).
pub fn measure_round_trip(recorded: &[f32], sample_rate: f32) -> Option<usize> {
    let click: Vec<f32> = (0..).map_while(|k| {
        let len = (0.002 * sample_rate).round().max(8.0) as usize;
        (k < len).then(|| calibration_click(k, sample_rate))
    }).collect();
    let energy: f32 = click.iter().map(|c| c * c).sum();
    let max_lag = (CALIBRATION_MAX_LAG * sample_rate) as usize;
    let mut lags = Vec::new();
    for &at in &CALIBRATION_CLICKS {
        let at = (at * sample_rate) as usize;
        let window = recorded.get(at..(at + max_lag + click.len()).min(recorded.len()))?;
        if window.len() <= click.len() {
            return None;
        }
        let corr = |lag: usize| -> f32 { click.iter().zip(&window[lag..]).map(|(c, x)| c * x).sum() };
        let (best, peak) = (0..window.len() - click.len())
            .map(|lag| (lag, corr(lag).abs()))
            .max_by(|a, b| a.1.total_cmp(&b.1))?;
        // Bruit de fond : énergie moyenne de la fenêtre sur la longueur du clic.
        let noise = (window.iter().map(|x| x * x).sum::<f32>() / window.len() as f32 * energy).sqrt();
        if peak < 4.0 * noise.max(1e-9) || peak < 0.002 * energy {
            return None;
        }
        lags.push(best);
    }
    lags.sort_unstable();
    let median = lags[lags.len() / 2];
    // Les clics doivent revenir avec le même retard (à 1 ms près).
    let spread = lags.iter().filter(|&&l| l.abs_diff(median) <= (0.001 * sample_rate) as usize).count();
    (spread * 2 > lags.len()).then_some(median)
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
    fn mono_take_continues_in_chunks_then_flattens() {
        let mut b = RecordBuffer::for_source(InputSource::Left, 1.0, 3.0);
        assert_eq!(b.capacity(), 3);
        b.chunks.push(Box::new(RecordChunk::new(2, true)));
        b.chunks.push(Box::new(RecordChunk::new(4, true)));
        assert_eq!((b.capacity(), b.room()), (9, 9));
        let src: Vec<f32> = (1..=8).map(|i| i as f32).collect();
        assert_eq!(b.write(&src[..4], &src[..4]), 4);
        assert_eq!(b.write(&src[4..], &src[4..]), 4);
        assert_eq!(b.room(), 1);
        b.flatten();
        assert_eq!(&b.left[..b.len], &src[..]);
        assert!(b.right.is_empty() && b.chunks.is_empty());
        let s = b.into_sample("take");
        assert_eq!((s.left.len(), s.right.len()), (8, 8));
    }

    #[test]
    fn deinterleave_pads_and_duplicates_mono() {
        let (mut l, mut r) = ([9.0; 4], [9.0; 4]);
        deinterleave(&[0.1, 0.2, 0.3], 1, &mut l, &mut r);
        assert_eq!((l, r), ([0.1, 0.2, 0.3, 0.0], [0.1, 0.2, 0.3, 0.0]));
        deinterleave(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0], 3, &mut l, &mut r);
        assert_eq!((l, r), ([1.0, 4.0, 0.0, 0.0], [2.0, 5.0, 0.0, 0.0]));
        deinterleave(&[], 0, &mut l, &mut r);
        assert_eq!(l, [0.0; 4]);
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

    #[test]
    fn mono_buffer_keeps_one_channel_and_plays_on_both() {
        let mut b = RecordBuffer::for_source(InputSource::Right, 1.0, 4.0);
        assert!(b.is_mono() && b.right.is_empty());
        assert_eq!(b.write(&[0.5; 2], &[0.7; 2]), 2);
        let s = b.into_sample("mono");
        assert_eq!((s.left.as_slice(), s.right.as_slice()), ([0.5, 0.5].as_slice(), [0.5, 0.5].as_slice()));
    }

    #[test]
    fn strip_gain_is_smoothed_and_clips_are_flagged() {
        let mut c = InputChain::new(48_000.0);
        c.set(InputStrip { gain_db: 6.0, ..Default::default() });
        let (mut l, mut r) = (vec![0.25; 4_800], vec![0.25; 4_800]);
        assert!(!c.process(&mut l, &mut r));
        assert!(l[0] < 0.26, "le gain monte en douceur : {}", l[0]);
        assert!((l[4_799] - 0.5).abs() < 0.01, "+6 dB : {}", l[4_799]);
        // Convertisseur à fond : saturation signalée même sans gain.
        let mut c = InputChain::new(48_000.0);
        assert!(c.process(&mut [1.0], &mut [0.0]));
    }

    #[test]
    fn high_pass_removes_rumble() {
        let mut c = InputChain::new(48_000.0);
        c.set(InputStrip { high_pass: true, ..Default::default() });
        let tone = |hz: f32| -> Vec<f32> {
            (0..48_000).map(|k| (std::f32::consts::TAU * hz * k as f32 / 48_000.0).sin() * 0.5).collect()
        };
        let peak_after = |c: &mut InputChain, mut x: Vec<f32>| {
            let mut y = x.clone();
            c.process(&mut x, &mut y);
            x[24_000..].iter().fold(0.0f32, |m, s| m.max(s.abs()))
        };
        assert!(peak_after(&mut c, tone(20.0)) < 0.05, "20 Hz coupé");
        assert!(peak_after(&mut c, tone(1_000.0)) > 0.49, "1 kHz intact");
    }

    #[test]
    fn gate_lowers_noise_and_opens_on_voice() {
        let mut c = InputChain::new(48_000.0);
        c.set(InputStrip { gate: true, gate_threshold_db: -40.0, ..Default::default() });
        // Souffle à −60 dBFS : atténué de 18 dB.
        let mut n: Vec<f32> = (0..48_000).map(|k| if k % 2 == 0 { 0.001 } else { -0.001 }).collect();
        let mut m = n.clone();
        c.process(&mut n, &mut m);
        assert!((c.gate_gain() - db_to_gain(GATE_RANGE_DB)).abs() < 0.01, "{}", c.gate_gain());
        // Voix à −12 dBFS : la porte s'ouvre en quelques millisecondes.
        let mut v = vec![0.25; 960];
        let mut w = v.clone();
        c.process(&mut v, &mut w);
        assert!(c.gate_gain() > 0.99, "{}", c.gate_gain());
        assert!((v[959] - 0.25).abs() < 0.01);
    }

    #[test]
    fn single_take_is_shifted_back_by_the_latency() {
        // 100 échantillons par tick ; 250 trames de retard : 2,5 ticks plus tôt.
        let s = take_slices(10_000, 96.0, &[], 250, 100.0, None, 1);
        assert_eq!(s, [TakeSlice { from: 0, to: 10_000, tick: 93.5, pass: 0 }]);
        // Clip au tick 94 : on saute la demi-tick (50 trames) dans le fichier.
        let (start, offset) = clip_placement(&s[0], 100.0, 1_000.0);
        assert_eq!(start, 94);
        assert!((offset - 0.05).abs() < 1e-12);
    }

    #[test]
    fn take_before_zero_is_trimmed() {
        let s = take_slices(1_000, 1.0, &[], 300, 100.0, None, 1);
        assert_eq!(s, [TakeSlice { from: 200, to: 1_000, tick: 0.0, pass: 0 }]);
    }

    #[test]
    fn loop_passes_become_aligned_takes() {
        // Boucle de 384 à 768 ticks, 10 trames par tick, 50 trames de retard.
        // Départ au tick 300 (pré-roll), sauts aux trames 4 680 et 8 520.
        let jumps = [(4_680, 384.0), (8_520, 384.0)];
        let s = take_slices(10_000, 300.0, &jumps, 50, 10.0, None, 1);
        assert_eq!(s.len(), 3);
        assert_eq!(s[0], TakeSlice { from: 0, to: 4_730, tick: 295.0, pass: 0 });
        // La fin du premier tour est dans les 50 premières trames après le saut.
        assert_eq!(s[1], TakeSlice { from: 4_730, to: 8_570, tick: 384.0, pass: 1 });
        assert_eq!(s[2], TakeSlice { from: 8_570, to: 10_000, tick: 384.0, pass: 2 });
        // Chaque tour complet fait pile la boucle.
        assert_eq!(s[1].to - s[1].from, 3_840);
    }

    #[test]
    fn punch_keeps_only_the_selection() {
        let jumps = [(4_680, 384.0), (8_520, 384.0)];
        let s = take_slices(10_000, 300.0, &jumps, 50, 10.0, Some((400.0, 700.0)), 1);
        assert_eq!(s.len(), 3);
        // Premier tour : de 400 à 700, soit 105 ticks après le début du tampon (tick 295).
        assert_eq!(s[0], TakeSlice { from: 1_050, to: 4_050, tick: 400.0, pass: 0 });
        assert_eq!(s[1], TakeSlice { from: 4_730 + 160, to: 4_730 + 3_160, tick: 400.0, pass: 1 });
        // Dernier tour, arrêté avant la fin de la sélection.
        assert_eq!(s[2], TakeSlice { from: 8_570 + 160, to: 10_000, tick: 400.0, pass: 2 });
        // Rien dans la sélection : aucune prise.
        assert!(take_slices(1_000, 0.0, &[], 0, 10.0, Some((500.0, 600.0)), 1).is_empty());
    }

    #[test]
    fn tiny_slices_are_dropped() {
        let s = take_slices(1_000, 0.0, &[(990, 0.0)], 0, 10.0, None, 64);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].to, 990);
    }

    #[test]
    fn round_trip_is_found_to_the_sample() {
        let sr = 48_000.0;
        for lag in [0usize, 37, 517, 9_001] {
            let mut rec = vec![0.0f32; (CALIBRATION_SECONDS * sr) as usize];
            // Retour atténué, avec un peu de souffle.
            for (i, s) in rec.iter_mut().enumerate() {
                *s = 0.002 * ((i * 7_919) % 13) as f32 / 13.0 - 0.001;
            }
            for &at in &CALIBRATION_CLICKS {
                let at = (at * sr) as usize + lag;
                for k in 0..200 {
                    rec[at + k] += 0.3 * calibration_click(k, sr);
                }
            }
            assert_eq!(measure_round_trip(&rec, sr), Some(lag));
        }
        // Silence : pas de mesure.
        assert_eq!(measure_round_trip(&vec![0.0; (CALIBRATION_SECONDS * sr) as usize], sr), None);
    }
}
