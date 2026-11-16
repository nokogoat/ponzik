//! Mesure de sonie UIT-R BS.1770-4 / EBU R128 : pondération K, sonie momentanée (400 ms), court terme (3 s),
//! intégrée (double porte : absolue à −70 LUFS, relative à −10 LU), plage de sonie (LRA, EBU Tech 3342) et
//! crête vraie (suréchantillonnage 4× polyphase, BS.1770-4 annexe 2).
//!
//! Tout est alloué à la construction. L'historique garde [`HISTORY_SECONDS`] secondes (2 h) de blocs de
//! 100 ms ; au-delà, les blocs les plus anciens sont remplacés et la sonie intégrée et la LRA portent sur les
//! 2 dernières heures.

/// Nombre maximal de canaux.
pub const MAX_CHANNELS: usize = 8;

/// Durée couverte par l'historique de la sonie intégrée et de la LRA.
pub const HISTORY_SECONDS: usize = 2 * 60 * 60;

/// Blocs mémorisés : un toutes les 100 ms.
pub const MAX_BLOCKS: usize = HISTORY_SECONDS * 10;

/// Sous-blocs de 100 ms dans la fenêtre court terme.
const SHORT_TERM_SEGMENTS: usize = 30;
const MOMENTARY_SEGMENTS: usize = 4;

const ABSOLUTE_GATE_LUFS: f64 = -70.0;
const RELATIVE_GATE_LU: f64 = -10.0;
const LRA_RELATIVE_GATE_LU: f64 = -20.0;

/// Sonie (LUFS) d'une énergie pondérée.
#[inline]
fn lufs(energy: f64) -> f32 {
    if energy > 0.0 { (-0.691 + 10.0 * energy.log10()) as f32 } else { f32::NEG_INFINITY }
}

/// Énergie pondérée correspondant à une sonie (LUFS).
#[inline]
fn energy(lufs: f64) -> f64 {
    10f64.powf((lufs + 0.691) / 10.0)
}

/// Coefficients normalisés (`a0 = 1`) d'un biquad en double précision.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Coefs64 {
    pub b: [f64; 3],
    pub a: [f64; 2],
}

/// Les deux étages de la pondération K pour une fréquence d'échantillonnage quelconque : plateau aigu (+4 dB,
/// effet de la tête) puis passe-haut RLB. Les coefficients redonnent ceux de la norme à 48 kHz.
pub fn k_weighting(sample_rate: f64) -> [Coefs64; 2] {
    let shelf = {
        let (f0, gain_db, q) = (1_681.974_450_955_533, 3.999_843_853_973_347, 0.707_175_236_955_419_6);
        let k = (std::f64::consts::PI * f0 / sample_rate).tan();
        let vh = 10f64.powf(gain_db / 20.0);
        let vb = vh.powf(0.499_666_774_154_541_6);
        let a0 = 1.0 + k / q + k * k;
        Coefs64 {
            b: [(vh + vb * k / q + k * k) / a0, 2.0 * (k * k - vh) / a0, (vh - vb * k / q + k * k) / a0],
            a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
        }
    };
    let highpass = {
        let (f0, q) = (38.135_470_876_024_44, 0.500_327_037_323_877_3);
        let k = (std::f64::consts::PI * f0 / sample_rate).tan();
        let a0 = 1.0 + k / q + k * k;
        Coefs64 { b: [1.0, -2.0, 1.0], a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0] }
    };
    [shelf, highpass]
}

/// Biquad en forme directe transposée II, double précision.
#[derive(Clone, Copy, Debug)]
struct Biquad64 {
    c: Coefs64,
    z1: f64,
    z2: f64,
}

impl Biquad64 {
    fn new(c: Coefs64) -> Self {
        Self { c, z1: 0.0, z2: 0.0 }
    }

    #[inline]
    fn process(&mut self, x: f64) -> f64 {
        let y = self.c.b[0] * x + self.z1;
        self.z1 = self.c.b[1] * x - self.c.a[0] * y + self.z2;
        self.z2 = self.c.b[2] * x - self.c.a[1] * y;
        y
    }

    fn reset(&mut self) {
        (self.z1, self.z2) = (0.0, 0.0);
    }
}

const TP_TAPS: usize = 12;

/// Filtre d'interpolation 4× de BS.1770-4 (annexe 2) : 48 coefficients en 4 phases de 12. Phases 0 et 1 en
/// multiples de 2^-16 (valeurs exactes de la norme) ; les phases 2 et 3 sont leurs miroirs.
const TP_PHASE_0: [f32; TP_TAPS] =
    [112.0, 720.0, -1288.0, 2176.0, -3896.0, 9000.0, 63712.0, -6704.0, 3120.0, -1744.0, 976.0, -544.0];
const TP_PHASE_1: [f32; TP_TAPS] =
    [-1912.0, 1920.0, -3392.0, 5840.0, -10912.0, 30480.0, 51104.0, -13128.0, 6656.0, -3816.0, 2168.0, -1240.0];

const fn tp_phases() -> [[f32; TP_TAPS]; 4] {
    let mut out = [[0.0; TP_TAPS]; 4];
    let mut j = 0;
    while j < TP_TAPS {
        out[0][j] = TP_PHASE_0[j] / 65536.0;
        out[1][j] = TP_PHASE_1[j] / 65536.0;
        out[2][j] = TP_PHASE_1[TP_TAPS - 1 - j] / 65536.0;
        out[3][j] = TP_PHASE_0[TP_TAPS - 1 - j] / 65536.0;
        j += 1;
    }
    out
}

const TP_PHASES: [[f32; TP_TAPS]; 4] = tp_phases();

/// Détecteur de crête vraie d'un canal : suréchantillonnage 4× polyphase, valeur absolue maximale.
///
/// Retard du filtre : 6 échantillons à la fréquence de base.
#[derive(Clone, Debug, Default)]
pub struct TruePeak {
    hist: [f32; 2 * TP_TAPS],
    pos: usize,
}

impl TruePeak {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ajoute un échantillon ; renvoie la plus grande valeur absolue des 4 échantillons interpolés.
    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        self.pos = if self.pos == 0 { TP_TAPS - 1 } else { self.pos - 1 };
        self.hist[self.pos] = x;
        self.hist[self.pos + TP_TAPS] = x;
        let window = &self.hist[self.pos..self.pos + TP_TAPS];
        TP_PHASES.iter().map(|h| h.iter().zip(window).map(|(c, x)| c * x).sum::<f32>().abs()).fold(0.0, f32::max)
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Historique circulaire d'énergies à capacité fixe.
#[derive(Clone, Debug)]
struct History {
    data: Vec<f32>,
    len: usize,
    pos: usize,
}

impl History {
    fn new(capacity: usize) -> Self {
        Self { data: vec![0.0; capacity], len: 0, pos: 0 }
    }

    fn push(&mut self, e: f64) {
        self.data[self.pos] = e as f32;
        self.pos = (self.pos + 1) % self.data.len();
        self.len = (self.len + 1).min(self.data.len());
    }

    fn values(&self) -> &[f32] {
        &self.data[..self.len]
    }

    fn clear(&mut self) {
        (self.len, self.pos) = (0, 0);
    }
}

/// Moyenne des énergies au-dessus de la porte `gate`, et leur nombre.
fn gated_mean(values: &[f32], gate: f64) -> (f64, usize) {
    let (sum, n) = values.iter().filter(|&&e| e as f64 > gate).fold((0.0, 0), |(s, n), &e| (s + e as f64, n + 1));
    (if n > 0 { sum / n as f64 } else { 0.0 }, n)
}

/// Sonie-mètre multicanal (jusqu'à [`MAX_CHANNELS`]).
#[derive(Clone, Debug)]
pub struct LoudnessMeter {
    channels: usize,
    weights: [f64; MAX_CHANNELS],
    filters: [[Biquad64; 2]; MAX_CHANNELS],
    peaks: [TruePeak; MAX_CHANNELS],
    acc: [f64; MAX_CHANNELS],
    segment_len: usize,
    segment_fill: usize,
    /// Énergies des 30 derniers sous-blocs de 100 ms.
    segments: [f64; SHORT_TERM_SEGMENTS],
    segment_count: u64,
    momentary: f64,
    max_momentary: f64,
    max_short_term: f64,
    /// Blocs de 400 ms (porte de la sonie intégrée).
    blocks: History,
    /// Fenêtres de 3 s (LRA).
    short_terms: History,
    scratch: Vec<f32>,
    true_peak: f32,
    sample_peak: f32,
}

impl LoudnessMeter {
    /// Poids par défaut : 1 pour les canaux 0 à 2 et 1,41 pour 3 et 4 (ordre G, D, C, Gs, Ds) ; autres
    /// dispositions (LFE à 0, par exemple) via [`Self::set_channel_weight`].
    pub fn new(sample_rate: f32, channels: usize) -> Self {
        assert!((1..=MAX_CHANNELS).contains(&channels), "nombre de canaux invalide : {channels}");
        let [shelf, highpass] = k_weighting(sample_rate as f64);
        let mut weights = [1.0; MAX_CHANNELS];
        weights[3..5].fill(1.41);
        Self {
            channels,
            weights,
            filters: [[Biquad64::new(shelf), Biquad64::new(highpass)]; MAX_CHANNELS],
            peaks: Default::default(),
            acc: [0.0; MAX_CHANNELS],
            segment_len: ((sample_rate as f64 * 0.1).round() as usize).max(1),
            segment_fill: 0,
            segments: [0.0; SHORT_TERM_SEGMENTS],
            segment_count: 0,
            momentary: 0.0,
            max_momentary: 0.0,
            max_short_term: 0.0,
            blocks: History::new(MAX_BLOCKS),
            short_terms: History::new(MAX_BLOCKS),
            scratch: Vec::with_capacity(MAX_BLOCKS),
            true_peak: 0.0,
            sample_peak: 0.0,
        }
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    pub fn set_channel_weight(&mut self, channel: usize, weight: f32) {
        self.weights[channel] = weight.max(0.0) as f64;
    }

    /// Remet toutes les mesures à zéro.
    pub fn reset(&mut self) {
        self.filters.iter_mut().flatten().for_each(Biquad64::reset);
        self.peaks.iter_mut().for_each(TruePeak::reset);
        self.acc = [0.0; MAX_CHANNELS];
        self.segment_fill = 0;
        self.segments = [0.0; SHORT_TERM_SEGMENTS];
        self.segment_count = 0;
        (self.momentary, self.max_momentary, self.max_short_term) = (0.0, 0.0, 0.0);
        self.blocks.clear();
        self.short_terms.clear();
        self.reset_peaks();
    }

    /// Remet à zéro les crêtes (vraie et échantillon) sans toucher à la sonie.
    pub fn reset_peaks(&mut self) {
        (self.true_peak, self.sample_peak) = (0.0, 0.0);
    }

    /// Traite un bloc planaire : une tranche par canal (canaux en trop ignorés, longueur commune la plus courte).
    pub fn process(&mut self, input: &[&[f32]]) {
        let channels = input.len().min(self.channels);
        let len = input[..channels].iter().map(|c| c.len()).min().unwrap_or(0);
        for i in 0..len {
            for (ch, samples) in input[..channels].iter().enumerate() {
                let x = samples[i];
                let [shelf, highpass] = &mut self.filters[ch];
                let y = highpass.process(shelf.process(x as f64));
                self.acc[ch] += y * y;
                self.sample_peak = self.sample_peak.max(x.abs());
                self.true_peak = self.true_peak.max(self.peaks[ch].process(x));
            }
            self.segment_fill += 1;
            if self.segment_fill == self.segment_len {
                self.close_segment();
            }
        }
    }

    pub fn process_stereo(&mut self, left: &[f32], right: &[f32]) {
        self.process(&[left, right]);
    }

    fn close_segment(&mut self) {
        let n = self.segment_len as f64;
        let e = (0..self.channels).map(|ch| self.weights[ch] * self.acc[ch] / n).sum::<f64>();
        self.acc = [0.0; MAX_CHANNELS];
        self.segment_fill = 0;
        self.segments[(self.segment_count % SHORT_TERM_SEGMENTS as u64) as usize] = e;
        self.segment_count += 1;
        if self.segment_count >= MOMENTARY_SEGMENTS as u64 {
            self.momentary = self.recent_energy(MOMENTARY_SEGMENTS);
            self.max_momentary = self.max_momentary.max(self.momentary);
            self.blocks.push(self.momentary);
        }
        if self.segment_count >= SHORT_TERM_SEGMENTS as u64 {
            let st = self.recent_energy(SHORT_TERM_SEGMENTS);
            self.max_short_term = self.max_short_term.max(st);
            self.short_terms.push(st);
        }
    }

    /// Énergie moyenne des `n` derniers sous-blocs.
    fn recent_energy(&self, n: usize) -> f64 {
        let last = self.segment_count as usize;
        (1..=n).map(|k| self.segments[(last - k) % SHORT_TERM_SEGMENTS]).sum::<f64>() / n as f64
    }

    /// Sonie momentanée (fenêtre de 400 ms, mise à jour toutes les 100 ms), en LUFS ; −∞ avant 400 ms.
    pub fn momentary(&self) -> f32 {
        if self.segment_count >= MOMENTARY_SEGMENTS as u64 { lufs(self.momentary) } else { f32::NEG_INFINITY }
    }

    /// Sonie court terme (fenêtre de 3 s), en LUFS ; entre 400 ms et 3 s, porte sur ce qui a été entendu.
    pub fn short_term(&self) -> f32 {
        let n = (self.segment_count as usize).min(SHORT_TERM_SEGMENTS);
        if n >= MOMENTARY_SEGMENTS { lufs(self.recent_energy(n)) } else { f32::NEG_INFINITY }
    }

    pub fn max_momentary(&self) -> f32 {
        lufs(self.max_momentary)
    }

    /// Maximum de la sonie court terme sur des fenêtres complètes de 3 s.
    pub fn max_short_term(&self) -> f32 {
        lufs(self.max_short_term)
    }

    /// Sonie intégrée à double porte (BS.1770-4), en LUFS ; −∞ si aucun bloc ne passe la porte absolue.
    /// Parcourt l'historique : à appeler depuis l'interface plutôt qu'à chaque bloc audio.
    pub fn integrated(&self) -> f32 {
        let values = self.blocks.values();
        let abs_gate = energy(ABSOLUTE_GATE_LUFS);
        let (mean, n) = gated_mean(values, abs_gate);
        if n == 0 {
            return f32::NEG_INFINITY;
        }
        let rel_gate = (mean * 10f64.powf(RELATIVE_GATE_LU / 10.0)).max(abs_gate);
        let (mean, n) = gated_mean(values, rel_gate);
        if n == 0 { f32::NEG_INFINITY } else { lufs(mean) }
    }

    /// Plage de sonie (EBU Tech 3342), en LU : écart entre les centiles 10 et 95 des sonies court terme
    /// passant les portes absolue (−70 LUFS) et relative (−20 LU). Utilise un tampon interne préalloué.
    pub fn loudness_range(&mut self) -> f32 {
        let abs_gate = energy(ABSOLUTE_GATE_LUFS);
        let (mean, n) = gated_mean(self.short_terms.values(), abs_gate);
        if n < 2 {
            return 0.0;
        }
        let rel_gate = (mean * 10f64.powf(LRA_RELATIVE_GATE_LU / 10.0)).max(abs_gate);
        self.scratch.clear();
        self.scratch.extend(self.short_terms.values().iter().filter(|&&e| e as f64 > rel_gate));
        if self.scratch.len() < 2 {
            return 0.0;
        }
        // L'énergie est croissante avec la sonie : trier les énergies revient à trier les sonies.
        self.scratch.sort_unstable_by(f32::total_cmp);
        let at = |p: f64| self.scratch[((self.scratch.len() - 1) as f64 * p).round() as usize] as f64;
        (10.0 * (at(0.95) / at(0.10)).log10()) as f32
    }

    /// Crête vraie maximale depuis le dernier `reset_peaks`, en dBTP.
    pub fn true_peak(&self) -> f32 {
        20.0 * self.true_peak.max(1e-10).log10()
    }

    /// Crête échantillon maximale depuis le dernier `reset_peaks`, en dBFS.
    pub fn sample_peak(&self) -> f32 {
        20.0 * self.sample_peak.max(1e-10).log10()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    fn sine(hz: f64, dbfs: f64, sr: f64, seconds: f64) -> Vec<f32> {
        let amp = 10f64.powf(dbfs / 20.0);
        (0..(sr * seconds) as usize).map(|i| (amp * (TAU * hz * i as f64 / sr).sin()) as f32).collect()
    }

    /// Joue une suite de sinus stéréo `(dBFS, secondes)` à 1 kHz.
    fn play(meter: &mut LoudnessMeter, sr: f64, parts: &[(f64, f64)]) {
        for &(db, s) in parts {
            let x = sine(1000.0, db, sr, s);
            for chunk in x.chunks(512) {
                meter.process_stereo(chunk, chunk);
            }
        }
    }

    fn near(value: f32, want: f32, tol: f32) {
        assert!((value - want).abs() <= tol, "{value} au lieu de {want} ± {tol}");
    }

    #[test]
    fn k_weighting_matches_bs1770_at_48k() {
        let [shelf, hp] = k_weighting(48_000.0);
        let want_shelf = [1.535_124_859_586_97, -2.691_696_189_406_38, 1.198_392_810_852_85];
        let want_shelf_a = [-1.690_659_293_182_41, 0.732_480_774_215_85];
        let want_hp_a = [-1.990_047_454_833_98, 0.990_072_250_366_21];
        for (got, want) in shelf.b.iter().zip(&want_shelf).chain(shelf.a.iter().zip(&want_shelf_a)) {
            assert!((got - want).abs() < 1e-8, "{got} au lieu de {want}");
        }
        assert_eq!(hp.b, [1.0, -2.0, 1.0]);
        for (got, want) in hp.a.iter().zip(&want_hp_a) {
            assert!((got - want).abs() < 1e-8, "{got} au lieu de {want}");
        }
    }

    #[test]
    fn stereo_sine_at_minus_23_reads_minus_23_lufs() {
        // EBU Tech 3341, cas 1 et 2, à plusieurs fréquences d'échantillonnage.
        for sr in [44_100.0, 48_000.0, 96_000.0] {
            for db in [-23.0, -33.0] {
                let mut m = LoudnessMeter::new(sr as f32, 2);
                play(&mut m, sr, &[(db, 20.0)]);
                near(m.momentary(), db as f32, 0.1);
                near(m.short_term(), db as f32, 0.1);
                near(m.integrated(), db as f32, 0.1);
                near(m.max_momentary(), db as f32, 0.1);
            }
        }
    }

    #[test]
    fn mono_sine_at_minus_20_reads_minus_23_lufs() {
        let mut m = LoudnessMeter::new(48_000.0, 1);
        let x = sine(1000.0, -20.0, 48_000.0, 10.0);
        m.process(&[&x]);
        near(m.momentary(), -23.0, 0.1);
        near(m.integrated(), -23.0, 0.1);
    }

    #[test]
    fn nothing_before_400_ms() {
        let mut m = LoudnessMeter::new(48_000.0, 2);
        play(&mut m, 48_000.0, &[(-23.0, 0.35)]);
        assert_eq!(m.momentary(), f32::NEG_INFINITY);
        assert_eq!(m.integrated(), f32::NEG_INFINITY);
        play(&mut m, 48_000.0, &[(-23.0, 0.1)]);
        assert!(m.momentary().is_finite());
    }

    #[test]
    fn relative_and_absolute_gates() {
        // EBU Tech 3341, cas 3 et 4.
        let mut m = LoudnessMeter::new(48_000.0, 2);
        play(&mut m, 48_000.0, &[(-36.0, 10.0), (-23.0, 60.0), (-36.0, 10.0)]);
        near(m.integrated(), -23.0, 0.1);
        let mut m = LoudnessMeter::new(48_000.0, 2);
        play(&mut m, 48_000.0, &[(-72.0, 10.0), (-36.0, 10.0), (-23.0, 60.0), (-36.0, 10.0), (-72.0, 10.0)]);
        near(m.integrated(), -23.0, 0.1);
        // Le silence seul ne passe pas la porte absolue.
        let mut m = LoudnessMeter::new(48_000.0, 2);
        play(&mut m, 48_000.0, &[(-200.0, 2.0)]);
        assert_eq!(m.integrated(), f32::NEG_INFINITY);
    }

    #[test]
    fn loudness_range_matches_tech_3342() {
        for (a, b, want) in [(-20.0, -30.0, 10.0), (-20.0, -15.0, 5.0), (-40.0, -20.0, 20.0)] {
            let mut m = LoudnessMeter::new(48_000.0, 2);
            play(&mut m, 48_000.0, &[(a, 20.0), (b, 20.0)]);
            near(m.loudness_range(), want, 1.0);
        }
    }

    #[test]
    fn true_peak_of_full_scale_997_hz() {
        let mut m = LoudnessMeter::new(48_000.0, 2);
        let x = sine(997.0, 0.0, 48_000.0, 1.0);
        m.process_stereo(&x, &x);
        near(m.true_peak(), 0.0, 0.1);
        near(m.sample_peak(), 0.0, 0.01);
    }

    #[test]
    fn inter_sample_peak_reads_above_sample_peak() {
        // Sinus à fs/4 déphasé de 45° : tous les échantillons valent ±0,707, la crête réelle vaut 1.
        let x: Vec<f32> = (0..48_000).map(|i| (TAU * i as f64 / 4.0 + TAU / 8.0).sin() as f32).collect();
        let mut m = LoudnessMeter::new(48_000.0, 1);
        m.process(&[&x]);
        near(m.sample_peak(), -3.01, 0.01);
        near(m.true_peak(), 0.0, 0.5);
        assert!(m.true_peak() > m.sample_peak() + 2.5);
        m.reset_peaks();
        assert!(m.true_peak() < -150.0);
    }

    #[test]
    fn reset_clears_everything() {
        let mut m = LoudnessMeter::new(48_000.0, 2);
        play(&mut m, 48_000.0, &[(-23.0, 4.0)]);
        assert!(m.integrated().is_finite() && m.max_short_term().is_finite());
        m.reset();
        assert_eq!(m.integrated(), f32::NEG_INFINITY);
        assert_eq!(m.short_term(), f32::NEG_INFINITY);
        assert_eq!(m.loudness_range(), 0.0);
    }
}
