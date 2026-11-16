//! Suggestion de l'assistant : d'une écoute ([`Analysis`]) et d'un profil de
//! source, des réglages pour chaque module de la chaîne.
//!
//! Le spectre mesuré est comparé à une courbe cible par profil (en puissance par
//! bande, relative au bruit rose), recalée sur la médiane de l'écart : seules les
//! formes comptent, pas le niveau. Les corrections restent douces (coupes jusqu'à
//! −6 dB, accentuations jusqu'à +3 dB) et ne portent que sur la plage utile de la
//! source. Pas d'allocation : appelable depuis n'importe quel thread.

use ponzik_dsp::biquad::BiquadCoefs;
use ponzik_dsp::util::gain_to_db;

use super::analysis::{Analysis, FLOOR_DB, N_BANDS, band_center};
use super::*;

/// Noms des profils, dans l'ordre de [`Profile`].
pub const PROFILES: &[&str] = &["Auto", "Vocal", "Drums", "Bass", "Synth/Keys", "Guitar", "Full mix"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    Auto,
    Vocal,
    Drums,
    Bass,
    Keys,
    Guitar,
    Mix,
}

const ALL: [Profile; 7] =
    [Profile::Auto, Profile::Vocal, Profile::Drums, Profile::Bass, Profile::Keys, Profile::Guitar, Profile::Mix];

impl Profile {
    pub fn from_index(i: usize) -> Self {
        ALL[i.min(ALL.len() - 1)]
    }

    pub fn index(self) -> usize {
        ALL.iter().position(|&p| p == self).unwrap_or(0)
    }

    pub fn name(self) -> &'static str {
        PROFILES[self.index()]
    }
}

/// Ce qu'on attend d'une source.
struct Spec {
    /// Courbe cible (Hz, dB), relative au bruit rose, interpolée en log-fréquence.
    target: &'static [(f32, f32)],
    /// Plage où l'EQ corrige.
    range: (f32, f32),
    /// Coupe-bas habituel (0 : aucun).
    high_pass: f32,
    low_shelf: f32,
    high_shelf: f32,
    /// Ratio, attaque et relâche de base du compresseur.
    comp: (f32, f32, f32),
    saturation: f32,
    de_ess: bool,
    /// Niveau efficace visé en sortie (dBFS).
    rms_target: f32,
}

fn spec(p: Profile) -> Spec {
    match p {
        Profile::Vocal => Spec {
            target: &[(20.0, -30.0), (80.0, -14.0), (150.0, -3.0), (300.0, 0.0), (2_500.0, 0.0), (5_000.0, -2.0), (10_000.0, -6.0), (20_000.0, -16.0)],
            range: (90.0, 16_000.0),
            high_pass: 80.0,
            low_shelf: 180.0,
            high_shelf: 10_000.0,
            comp: (3.0, 0.005, 0.08),
            saturation: 0.12,
            de_ess: true,
            rms_target: -18.0,
        },
        Profile::Drums => Spec {
            target: &[(20.0, -10.0), (50.0, 0.0), (120.0, 0.0), (400.0, -3.0), (1_000.0, -3.0), (5_000.0, -3.0), (12_000.0, -6.0), (20_000.0, -14.0)],
            range: (35.0, 16_000.0),
            high_pass: 30.0,
            low_shelf: 90.0,
            high_shelf: 9_000.0,
            comp: (4.0, 0.02, 0.12),
            saturation: 0.2,
            de_ess: false,
            rms_target: -18.0,
        },
        Profile::Bass => Spec {
            target: &[(20.0, -10.0), (40.0, 0.0), (150.0, 0.0), (400.0, -6.0), (1_000.0, -12.0), (4_000.0, -20.0), (10_000.0, -30.0), (20_000.0, -40.0)],
            range: (30.0, 5_000.0),
            high_pass: 28.0,
            low_shelf: 80.0,
            high_shelf: 3_000.0,
            comp: (4.0, 0.01, 0.12),
            saturation: 0.25,
            de_ess: false,
            rms_target: -18.0,
        },
        Profile::Keys | Profile::Auto => Spec {
            target: &[(20.0, -18.0), (60.0, -6.0), (150.0, 0.0), (1_000.0, 0.0), (4_000.0, -3.0), (10_000.0, -7.0), (20_000.0, -16.0)],
            range: (50.0, 16_000.0),
            high_pass: 50.0,
            low_shelf: 150.0,
            high_shelf: 9_000.0,
            comp: (2.0, 0.015, 0.15),
            saturation: 0.1,
            de_ess: false,
            rms_target: -18.0,
        },
        Profile::Guitar => Spec {
            target: &[(20.0, -24.0), (80.0, -8.0), (200.0, 0.0), (2_000.0, 0.0), (5_000.0, -3.0), (10_000.0, -10.0), (20_000.0, -20.0)],
            range: (80.0, 12_000.0),
            high_pass: 80.0,
            low_shelf: 200.0,
            high_shelf: 8_000.0,
            comp: (3.0, 0.01, 0.12),
            saturation: 0.15,
            de_ess: false,
            rms_target: -18.0,
        },
        Profile::Mix => Spec {
            target: &[(20.0, -6.0), (40.0, 1.0), (100.0, 1.5), (300.0, 0.0), (1_000.0, -1.0), (3_000.0, -2.5), (8_000.0, -4.5), (14_000.0, -8.0), (20_000.0, -16.0)],
            range: (30.0, 16_000.0),
            high_pass: 22.0,
            low_shelf: 110.0,
            high_shelf: 9_000.0,
            comp: (2.0, 0.03, 0.2),
            saturation: 0.05,
            de_ess: false,
            rms_target: -14.0,
        },
    }
}

/// Courbe cible du profil à `hz` (dB relatifs au bruit rose, 0 vers 1 kHz).
pub fn target_db(profile: Profile, hz: f32) -> f32 {
    let t = spec(profile).target;
    let x = hz.max(1.0).log2();
    if x <= t[0].0.log2() {
        return t[0].1;
    }
    for w in t.windows(2) {
        let (x0, x1) = (w[0].0.log2(), w[1].0.log2());
        if x <= x1 {
            let u = (x - x0) / (x1 - x0);
            return w[0].1 + u * (w[1].1 - w[0].1);
        }
    }
    t[t.len() - 1].1
}

/// Part de l'énergie (puissance par bande) entre `lo` et `hi`.
fn share(bands: &[f32; N_BANDS], lo: f32, hi: f32) -> f32 {
    let (mut part, mut total) = (0.0f32, 0.0f32);
    for (i, &d) in bands.iter().enumerate() {
        let p = 10f32.powf(d / 10.0);
        total += p;
        let f = band_center(i);
        if f >= lo && f < hi {
            part += p;
        }
    }
    if total > 0.0 { part / total } else { 0.0 }
}

/// Devine la source d'après le spectre, la dynamique et l'image stéréo.
pub fn detect(a: &Analysis) -> Profile {
    let low = share(&a.bands, 20.0, 150.0);
    let high = share(&a.bands, 5_000.0, 20_000.0);
    let (mut num, mut den) = (0.0f32, 0.0f32);
    for (i, &d) in a.bands.iter().enumerate() {
        let p = 10f32.powf(d / 10.0);
        num += p * band_center(i).log2();
        den += p;
    }
    let centroid = if den > 0.0 { 2f32.powf(num / den) } else { 1_000.0 };
    let stereo = a.side_db > -15.0;
    if a.crest_db > 13.0 && a.range_db > 8.0 {
        Profile::Drums
    } else if low > 0.6 && high < 0.02 {
        Profile::Bass
    } else if stereo && low > 0.12 && high > 0.02 {
        Profile::Mix
    } else if !stereo && a.sibilance_db > -16.0 && (250.0..4_000.0).contains(&centroid) && low < 0.2 {
        Profile::Vocal
    } else if !stereo && (300.0..2_500.0).contains(&centroid) && high < 0.03 {
        Profile::Guitar
    } else {
        Profile::Keys
    }
}

/// Réglages proposés pour les modules (ids [`P_HP_ON`] à [`P_OUTPUT`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Suggestion {
    /// Profil utilisé (le profil détecté si la demande était « Auto »).
    pub profile: Profile,
    pub values: [(u32, f32); SUGGESTED],
}

pub const SUGGESTED: usize = (P_OUTPUT - P_HP_ON + 1) as usize;

impl Suggestion {
    pub fn get(&self, id: u32) -> f32 {
        self.values.iter().find(|(i, _)| *i == id).map_or(0.0, |v| v.1)
    }

    fn set(&mut self, id: u32, v: f32) {
        if let Some(s) = self.values.iter_mut().find(|(i, _)| *i == id) {
            s.1 = PARAMS[id as usize].clamp(v);
        }
    }
}

/// Fréquence de travail des courbes de la suggestion.
const SR: f32 = 48_000.0;

fn mag_db(c: &BiquadCoefs, hz: f32) -> f32 {
    gain_to_db(c.magnitude(hz, SR))
}

/// Arrondis lisibles : 0,1 dB, 3 chiffres significatifs pour les fréquences.
fn round_db(d: f32) -> f32 {
    (d * 10.0).round() / 10.0
}

fn round_hz(f: f32) -> f32 {
    let step = 10f32.powf((f.log10().floor() - 1.0).max(0.0));
    (f / step).round() * step
}

/// Écoute + profil → réglages proposés.
pub fn suggest(a: &Analysis, requested: Profile) -> Suggestion {
    let profile = if requested == Profile::Auto { detect(a) } else { requested };
    let sp = spec(profile);
    let mut s = Suggestion { profile, values: std::array::from_fn(|k| (P_HP_ON + k as u32, PARAMS[P_HP_ON as usize + k].default)) };
    let centers: [f32; N_BANDS] = std::array::from_fn(band_center);

    // --- Écart à la cible, dans la plage utile ---
    let top = a.bands.iter().copied().fold(FLOOR_DB, f32::max);
    let useful: [bool; N_BANDS] = std::array::from_fn(|i| {
        let f = centers[i];
        a.bands[i] > top - 45.0 && f >= sp.range.0 && f <= sp.range.1
    });
    // Recalage sur la médiane de l'écart au cœur de la plage, où les cibles sont les plus sûres.
    let core = (sp.range.0.max(150.0), sp.range.1.min(5_000.0));
    let mut dev = [0.0f32; N_BANDS];
    let mut sorted = [0.0f32; N_BANDS];
    let mut n = 0;
    for i in 0..N_BANDS {
        if useful[i] {
            dev[i] = a.bands[i] - target_db(profile, centers[i]);
            if centers[i] >= core.0 && centers[i] <= core.1 {
                sorted[n] = dev[i];
                n += 1;
            }
        }
    }
    if n > 0 {
        sorted[..n].sort_unstable_by(f32::total_cmp);
        let offset = sorted[n / 2];
        for i in 0..N_BANDS {
            dev[i] = if useful[i] { dev[i] - offset } else { 0.0 };
        }
    }
    // Lissage sur une demi-octave, cinq sixièmes sous 250 Hz où les harmoniques
    // d'une note tenue se distinguent bande par bande.
    let mut r = [0.0f32; N_BANDS];
    for i in 0..N_BANDS {
        if !useful[i] {
            continue;
        }
        let half = if centers[i] < 250.0 { 2 } else { 1 };
        let (mut sum, mut k) = (0.0, 0);
        for j in i.saturating_sub(half)..=(i + half).min(N_BANDS - 1) {
            if useful[j] {
                sum += dev[j];
                k += 1;
            }
        }
        r[i] = sum / k as f32;
    }
    let apply = |r: &mut [f32; N_BANDS], c: &BiquadCoefs| {
        for i in 0..N_BANDS {
            if useful[i] {
                r[i] += mag_db(c, centers[i]);
            }
        }
    };
    let mean_in = |r: &[f32; N_BANDS], lo: f32, hi: f32| {
        let (mut sum, mut k) = (0.0, 0);
        for i in 0..N_BANDS {
            if useful[i] && centers[i] >= lo && centers[i] <= hi {
                sum += r[i];
                k += 1;
            }
        }
        if k == 0 { 0.0 } else { sum / k as f32 }
    };
    let gentle = |excess: f32| if excess > 0.0 { (-0.6 * excess).max(-4.0) } else { (-0.6 * excess).min(3.0) };

    // --- Plateaux ---
    let ls = mean_in(&r, sp.range.0, sp.low_shelf);
    let ls_gain = if ls.abs() > 1.5 { round_db(gentle(ls)) } else { 0.0 };
    if ls_gain != 0.0 {
        apply(&mut r, &BiquadCoefs::low_shelf(sp.low_shelf, 0.707, ls_gain, SR));
    }
    let hs = mean_in(&r, sp.high_shelf, sp.range.1);
    let hs_gain = if hs.abs() > 1.5 { round_db(gentle(hs)) } else { 0.0 };
    if hs_gain != 0.0 {
        apply(&mut r, &BiquadCoefs::high_shelf(sp.high_shelf, 0.707, hs_gain, SR));
    }
    s.set(P_BAND, sp.low_shelf);
    s.set(P_BAND + 1, ls_gain);
    s.set(P_BAND + 2, 0.707);
    s.set(P_BAND + 12, sp.high_shelf);
    s.set(P_BAND + 13, hs_gain);
    s.set(P_BAND + 14, 0.707);

    // --- Trois cloches : le plus grand écart restant, trois fois ---
    let mut bells = [(0.0f32, 0.0f32, 1.0f32); 3];
    let mut found = 0;
    let (lo, hi) = (sp.range.0.max(60.0), sp.range.1.min(12_000.0));
    for bell in &mut bells {
        let score = |x: f32| if x > 0.0 { x } else { -0.6 * x };
        let best = (0..N_BANDS)
            .filter(|&i| useful[i] && centers[i] >= lo && centers[i] <= hi)
            .max_by(|&i, &j| score(r[i]).total_cmp(&score(r[j])));
        let Some(i) = best else { break };
        if score(r[i]) < 1.5 {
            break;
        }
        // Largeur : les voisines du même signe au moins à mi-hauteur.
        let half = r[i].abs() * 0.5;
        let same = |j: usize| useful[j] && r[j].signum() == r[i].signum() && r[j].abs() >= half;
        let (mut left, mut right) = (i, i);
        while left > 0 && same(left - 1) {
            left -= 1;
        }
        while right + 1 < N_BANDS && same(right + 1) {
            right += 1;
        }
        // Au moins une demi-octave ; les accentuations restent larges.
        let octaves = ((right - left + 1).max(3) as f32) / 6.0;
        let max_q = if r[i] > 0.0 { 2.5 } else { 1.2 };
        let q = (2f32.powf(octaves / 2.0) / (2f32.powf(octaves) - 1.0)).clamp(0.5, max_q);
        let gain = if r[i] > 0.0 { (-0.75 * r[i]).max(-6.0) } else { (-0.5 * r[i]).min(3.0) };
        let (hz, gain, q) = (round_hz(centers[i]), round_db(gain), (q * 100.0).round() / 100.0);
        apply(&mut r, &BiquadCoefs::peaking(hz, q, gain, SR));
        *bell = (hz, gain, q);
        found += 1;
    }
    // Rangées par fréquence ; les cloches inutilisées gardent leurs défauts, à plat.
    bells[..found].sort_unstable_by(|x, y| x.0.total_cmp(&y.0));
    for (k, &(hz, gain, q)) in bells[..found].iter().enumerate() {
        let id = P_BAND + 3 * (k as u32 + 1);
        s.set(id, hz);
        s.set(id + 1, gain);
        s.set(id + 2, q);
    }
    let eq_on = ls_gain != 0.0 || hs_gain != 0.0 || found > 0;
    s.set(P_EQ_ON, eq_on as u8 as f32);

    // --- Coupe-bas : sous le contenu utile, jamais au-dessus de l'habitude du profil ---
    let reference = mean_level(&a.bands, 100.0, 1_000.0);
    let lowest = (0..N_BANDS).find(|&i| a.bands[i] > reference - 20.0).map_or(sp.high_pass, |i| centers[i]);
    let hp = sp.high_pass.min(lowest * 0.7).max(20.0);
    let hp_on = sp.high_pass > 0.0 && hp > 21.0;
    s.set(P_HP_ON, hp_on as u8 as f32);
    s.set(P_HP_FREQ, round_hz(if hp_on { hp } else { 80.0 }));

    // --- De-esser ---
    let de_ess = sp.de_ess && a.sibilance_db > -16.0 && a.sib_spread_db > 6.0;
    s.set(P_DS_ON, 1.0);
    if de_ess {
        let peak = (0..N_BANDS)
            .filter(|&i| (4_500.0..10_000.0).contains(&centers[i]))
            .max_by(|&i, &j| a.bands[i].total_cmp(&a.bands[j]))
            .map_or(6_500.0, |i| centers[i]);
        s.set(P_DS_FREQ, round_hz((peak * 0.8).clamp(4_000.0, 9_000.0)));
        s.set(P_DS_THRESHOLD, round_db((a.sib_level_db - 3.0).clamp(-60.0, 0.0)));
        s.set(P_DS_RANGE, round_db((3.0 + 0.6 * (a.sibilance_db + 14.0)).clamp(3.0, 10.0)));
    }

    // --- Compresseur : plus la source est dynamique, plus il travaille ---
    let (base_ratio, base_attack, base_release) = sp.comp;
    let by_range = ((a.range_db - 4.0) / 16.0).clamp(0.0, 1.0);
    let by_crest = ((a.crest_db - 9.0) / 9.0).clamp(0.0, 1.0);
    let dynamic = by_range.max(0.6 * by_crest);
    let ratio = 1.0 + (base_ratio - 1.0) * (0.2 + 0.8 * dynamic);
    let [p10, p25, p50, p75, p90] = a.peaks_db;
    let _ = p10;
    // Seuil entre les passages faibles et forts : on rapproche les forts des faibles.
    // Le détecteur suit les crêtes avec sa relâche : il se tient sous la crête des blocs.
    let mut threshold = (p25 + p75) * 0.5 - a.crest_db / 3.0;
    let slope = 1.0 - 1.0 / ratio;
    if (p90 - threshold) * slope > 10.0 {
        threshold = p90 - 10.0 / slope;
    }
    let threshold = threshold.clamp(-50.0, -3.0);
    let est_gr = [p25, p50, p75, p90].iter().map(|&p| comp_curve(p, threshold, ratio)).sum::<f32>() / 4.0;
    let makeup = (0.5 * est_gr).clamp(0.0, 12.0);
    s.set(P_COMP_ON, 1.0);
    s.set(P_COMP_THRESHOLD, round_db(threshold));
    s.set(P_COMP_RATIO, (ratio * 10.0).round() / 10.0);
    // Sons percussifs : attaque plus lente pour laisser passer les transitoires.
    s.set(P_COMP_ATTACK, base_attack * (1.0 + 0.5 * by_crest));
    s.set(P_COMP_RELEASE, base_release);
    s.set(P_COMP_MAKEUP, round_db(makeup));

    // --- Saturation : moins sur une source déjà dense ---
    let drive = sp.saturation * (0.5 + 0.5 * ((a.crest_db - 6.0) / 8.0).clamp(0.0, 1.0));
    s.set(P_SAT_ON, 1.0);
    s.set(P_SAT_DRIVE, (drive * 100.0).round() / 100.0);

    // --- Largeur ---
    let mono = a.side_db < -30.0;
    let width = if mono {
        1.0
    } else {
        match profile {
            Profile::Vocal if a.side_db > -6.0 => 0.8,
            Profile::Bass if a.side_db > -20.0 => 0.6,
            Profile::Drums => 1.1,
            Profile::Keys if a.side_db < -10.0 => 1.25,
            Profile::Guitar => 1.1,
            Profile::Mix if a.correlation < 0.3 => 0.85,
            Profile::Mix if a.side_db < -14.0 => 1.15,
            _ => 1.0,
        }
    };
    s.set(P_WIDTH_ON, 1.0);
    s.set(P_WIDTH, width);

    // --- Sortie : niveau visé, sans dépasser −1 dBFS en crête ---
    let get = |id: u32| s.get(id);
    let (mut before, mut after) = (0.0f32, 0.0f32);
    for (&d, &hz) in a.bands.iter().zip(&centers) {
        let p = 10f32.powf(d / 10.0);
        before += p;
        after += p * 10f32.powf(chain_response_db(&get, 1.0, hz, SR) / 10.0);
    }
    let eq_change = if before > 0.0 { 10.0 * (after / before).max(1e-6).log10() } else { 0.0 };
    let level = a.rms_db + eq_change - est_gr + makeup;
    let peak = a.peak_db + eq_change.max(0.0) - comp_curve(a.peak_db, threshold, ratio) + makeup;
    let out = (sp.rms_target - level).min(-1.0 - peak).clamp(-12.0, 12.0);
    s.set(P_OUTPUT, round_db(out));
    s
}

/// Niveau moyen (dB, en puissance) des bandes entre `lo` et `hi`.
fn mean_level(bands: &[f32; N_BANDS], lo: f32, hi: f32) -> f32 {
    let (mut sum, mut k) = (0.0f32, 0);
    for (i, &d) in bands.iter().enumerate() {
        let f = band_center(i);
        if f >= lo && f <= hi {
            sum += 10f32.powf(d / 10.0);
            k += 1;
        }
    }
    if k == 0 || sum <= 0.0 { FLOOR_DB } else { 10.0 * (sum / k as f32).log10() }
}
