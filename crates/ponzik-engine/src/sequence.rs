//! Données de séquence : notes, patterns, clips de playlist, automation et clips
//! audio. Le moteur en reçoit un instantané complet à chaque modification et y
//! cherche les notes à jouer.

use std::sync::Arc;

use crate::sample::Sample;

/// Résolution : ticks par noire.
pub const PPQ: u32 = 96;
/// Une double-croche, le pas du step sequencer.
pub const TICKS_PER_STEP: u32 = PPQ / 4;
/// Une mesure en 4/4.
pub const TICKS_PER_BAR: u32 = PPQ * 4;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Note {
    /// Début en ticks depuis le début du pattern.
    pub start: u32,
    /// Durée en ticks.
    pub len: u32,
    pub key: u8,
    /// Dans [0, 1].
    pub vel: f32,
}

/// Un pattern contient les notes de tous les canaux, comme dans FL Studio.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pattern {
    /// Longueur en ticks (la boucle du pattern).
    pub len: u32,
    /// Notes par canal, triées par début.
    pub notes: Vec<Vec<Note>>,
}

impl Pattern {
    pub fn new(len: u32, channels: usize) -> Self {
        Self { len, notes: vec![Vec::new(); channels] }
    }

    pub fn sort(&mut self) {
        for notes in &mut self.notes {
            notes.sort_by_key(|n| (n.start, n.key));
        }
    }
}

/// Occurrence d'un pattern dans la playlist.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Clip {
    pub pattern: usize,
    pub start: u32,
    pub len: u32,
    /// Décalage dans le pattern au début du clip (clip rogné à gauche).
    pub offset: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayMode {
    /// Boucle sur un pattern (mode pattern de FL Studio).
    Pattern(usize),
    /// Joue la playlist.
    Song,
}

/// Ce que pilote une courbe d'automation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutoTarget {
    InstrumentParam { channel: usize, id: u32 },
    EffectParam { track: usize, slot: usize, id: u32 },
    ChannelGain(usize),
    ChannelPan(usize),
    TrackGain(usize),
    TrackPan(usize),
    /// En BPM.
    Tempo,
}

/// Portée d'une courbe d'automation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutoScope {
    /// Sur la ligne de temps du morceau, en mode morceau seulement.
    Song,
    /// Relative au pattern : boucle avec lui en mode pattern et suit chacun de
    /// ses clips en mode morceau (automation de pattern façon FL Studio).
    Pattern(usize),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AutoPoint {
    pub tick: u32,
    /// En unités réelles du paramètre.
    pub value: f32,
    /// Tension du segment qui part de ce point, dans [-1, 1] ; 0 = linéaire.
    pub curve: f32,
}

/// Courbe d'automation : points triés par tick, valeur tenue avant le premier
/// et après le dernier.
#[derive(Clone, Debug, PartialEq)]
pub struct AutomationLane {
    pub target: AutoTarget,
    pub scope: AutoScope,
    pub points: Vec<AutoPoint>,
}

impl AutomationLane {
    pub fn value_at(&self, tick: f64) -> Option<f32> {
        automation_value(&self.points, tick)
    }
}

/// Mise en forme d'un segment : `t` dans [0, 1], tension dans [-1, 1].
/// Tension positive : départ lent (courbe creusée) ; négative : départ rapide.
pub fn shape_curve(t: f64, curve: f32) -> f64 {
    let t = t.clamp(0.0, 1.0);
    let c = curve.clamp(-1.0, 1.0) as f64;
    if c.abs() < 1e-6 {
        return t;
    }
    // Courbe de puissance, exposant de 1/5 à 5.
    t.powf(5f64.powf(c))
}

/// Valeur d'une suite de points triés à `tick` ; `None` sans point.
pub fn automation_value(points: &[AutoPoint], tick: f64) -> Option<f32> {
    let first = points.first()?;
    let next = points.partition_point(|p| p.tick as f64 <= tick);
    if next == 0 {
        return Some(first.value);
    }
    let a = points[next - 1];
    let Some(b) = points.get(next) else { return Some(a.value) };
    let span = b.tick as f64 - a.tick as f64;
    if span <= 0.0 {
        return Some(b.value);
    }
    let t = shape_curve((tick - a.tick as f64) / span, a.curve);
    Some((a.value as f64 + (b.value as f64 - a.value as f64) * t) as f32)
}

/// Échantillon posé dans la playlist, joué à sa vitesse d'origine.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioClip {
    pub sample: Arc<Sample>,
    /// Piste de mixer de destination.
    pub track: usize,
    /// Début en ticks.
    pub start: u32,
    /// Durée en ticks.
    pub len: u32,
    /// Décalage dans l'échantillon au début du clip, en secondes.
    pub offset: f64,
    pub gain: f32,
    /// Fondus en secondes (au moins 2 ms, contre les clics).
    pub fade_in: f32,
    pub fade_out: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Sequence {
    pub patterns: Vec<Pattern>,
    pub clips: Vec<Clip>,
    pub mode: PlayMode,
    /// Fin de la playlist en ticks (le morceau boucle ensuite).
    pub song_len: u32,
    /// Retard des doubles-croches paires, dans [0, 1] (1 = un demi-pas).
    pub swing: f32,
    pub automation: Vec<AutomationLane>,
    /// Joués en mode morceau seulement.
    pub audio_clips: Vec<AudioClip>,
    /// Boucle de lecture (début, fin) en ticks, tirée dans une règle.
    pub loop_range: Option<(u32, u32)>,
}

impl Default for Sequence {
    fn default() -> Self {
        Self {
            patterns: Vec::new(),
            clips: Vec::new(),
            mode: PlayMode::Pattern(0),
            song_len: TICKS_PER_BAR,
            swing: 0.0,
            automation: Vec::new(),
            audio_clips: Vec::new(),
            loop_range: None,
        }
    }
}

/// Note à déclencher, en position absolue.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SeqEvent {
    pub tick: f64,
    pub channel: usize,
    pub key: u8,
    pub vel: f32,
    pub len: u32,
}

impl Sequence {
    /// Boucle explicite valide (non vide).
    fn active_loop(&self) -> Option<(u32, u32)> {
        self.loop_range.filter(|(a, b)| b > a)
    }

    /// Début de la boucle du transport.
    pub fn loop_start(&self) -> f64 {
        self.active_loop().map_or(0.0, |(a, _)| a as f64)
    }

    /// Fin de boucle du transport : la boucle tirée dans la règle s'il y en a une,
    /// sinon la playlist boucle et un pattern est infini.
    pub fn loop_end(&self) -> Option<f64> {
        if let Some((_, b)) = self.active_loop() {
            return Some(b as f64);
        }
        match self.mode {
            PlayMode::Song => Some(self.song_len.max(TICKS_PER_BAR) as f64),
            PlayMode::Pattern(_) => None,
        }
    }

    /// Longueur affichée de la boucle courante, en ticks.
    pub fn cycle_len(&self) -> u32 {
        match self.mode {
            PlayMode::Song => self.song_len.max(TICKS_PER_BAR),
            PlayMode::Pattern(p) => self.patterns.get(p).map_or(TICKS_PER_BAR, |p| p.len.max(1)),
        }
    }

    /// Position locale d'une courbe de portée `scope` quand le transport est à
    /// `pos`, ou `None` si elle ne s'applique pas ici. En mode morceau, une
    /// courbe de pattern suit le dernier clip de ce pattern qui couvre `pos`.
    pub fn automation_tick(&self, scope: AutoScope, pos: f64) -> Option<f64> {
        let pattern_len = |p: usize| Some(self.patterns.get(p)?.len.max(1) as f64);
        match (scope, self.mode) {
            (AutoScope::Song, PlayMode::Song) => Some(pos),
            (AutoScope::Song, PlayMode::Pattern(_)) => None,
            (AutoScope::Pattern(p), PlayMode::Pattern(cur)) if p == cur => Some(pos.rem_euclid(pattern_len(p)?)),
            (AutoScope::Pattern(_), PlayMode::Pattern(_)) => None,
            (AutoScope::Pattern(p), PlayMode::Song) => {
                let len = pattern_len(p)?;
                let clip = self
                    .clips
                    .iter()
                    .rfind(|c| c.pattern == p && pos >= c.start as f64 && pos < c.start as f64 + c.len as f64)?;
                Some((pos - clip.start as f64 + clip.offset as f64).rem_euclid(len))
            }
        }
    }

    fn swing_offset(&self, start: u32) -> f64 {
        if start % (2 * TICKS_PER_STEP) == TICKS_PER_STEP {
            self.swing.clamp(0.0, 1.0) as f64 * TICKS_PER_STEP as f64 * 0.5
        } else {
            0.0
        }
    }

    /// Remplit `out` avec les notes qui commencent dans [`from`, `to`), triées.
    pub fn collect(&self, from: f64, to: f64, out: &mut Vec<SeqEvent>) {
        out.clear();
        match self.mode {
            PlayMode::Pattern(p) => {
                if let Some(pat) = self.patterns.get(p) {
                    self.collect_looped(pat, 0.0, from, to, out);
                }
            }
            PlayMode::Song => {
                for clip in &self.clips {
                    let Some(pat) = self.patterns.get(clip.pattern) else { continue };
                    let a = from.max(clip.start as f64);
                    let b = to.min((clip.start + clip.len) as f64);
                    if a < b {
                        self.collect_looped(pat, clip.start as f64 - clip.offset as f64, a, b, out);
                    }
                }
            }
        }
        out.sort_unstable_by(|a, b| a.tick.total_cmp(&b.tick).then(a.channel.cmp(&b.channel)));
    }

    /// Notes d'un pattern répété à partir de `origin`, dans [`from`, `to`).
    fn collect_looped(&self, pat: &Pattern, origin: f64, from: f64, to: f64, out: &mut Vec<SeqEvent>) {
        let len = pat.len.max(1) as f64;
        let max_swing = TICKS_PER_STEP as f64 * 0.5;
        // Un cycle plus tôt : le swing peut faire déborder une note dans la fenêtre.
        let mut base = origin + ((from - origin) / len).floor() * len - len;
        while base < to {
            let lo = (from - base - max_swing).max(0.0);
            let hi = to - base;
            for (channel, notes) in pat.notes.iter().enumerate() {
                let first = notes.partition_point(|n| (n.start as f64) < lo);
                for n in &notes[first..] {
                    if n.start as f64 >= hi || n.start >= pat.len {
                        break;
                    }
                    let tick = base + n.start as f64 + self.swing_offset(n.start);
                    if tick >= from && tick < to && out.len() < out.capacity().max(1) {
                        out.push(SeqEvent { tick, channel, key: n.key, vel: n.vel, len: n.len });
                    }
                }
            }
            base += len;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(start: u32) -> Note {
        Note { start, len: TICKS_PER_STEP, key: 60, vel: 1.0 }
    }

    fn pattern(starts: &[u32], len: u32) -> Pattern {
        let mut p = Pattern::new(len, 1);
        p.notes[0] = starts.iter().map(|&s| note(s)).collect();
        p
    }

    fn ticks(seq: &Sequence, from: f64, to: f64) -> Vec<f64> {
        let mut out = Vec::with_capacity(64);
        seq.collect(from, to, &mut out);
        out.iter().map(|e| e.tick).collect()
    }

    #[test]
    fn pattern_mode_loops() {
        let seq = Sequence { patterns: vec![pattern(&[0, 96], 192)], ..Default::default() };
        assert_eq!(ticks(&seq, 0.0, 192.0), [0.0, 96.0]);
        assert_eq!(ticks(&seq, 150.0, 300.0), [192.0, 288.0]);
        assert_eq!(ticks(&seq, 96.0, 96.5), [96.0]);
        assert!(ticks(&seq, 96.5, 191.0).is_empty());
    }

    #[test]
    fn song_mode_clips_loop_and_offset() {
        let seq = Sequence {
            patterns: vec![pattern(&[0, 48], 96)],
            clips: vec![
                Clip { pattern: 0, start: 0, len: 192, offset: 0 },
                Clip { pattern: 0, start: 384, len: 48, offset: 48 },
            ],
            mode: PlayMode::Song,
            song_len: 768,
            ..Default::default()
        };
        assert_eq!(ticks(&seq, 0.0, 768.0), [0.0, 48.0, 96.0, 144.0, 384.0]);
        assert_eq!(seq.loop_end(), Some(768.0));
    }

    #[test]
    fn swing_delays_even_sixteenths() {
        let seq = Sequence { patterns: vec![pattern(&[0, 24, 48], 96)], swing: 1.0, ..Default::default() };
        assert_eq!(ticks(&seq, 0.0, 96.0), [0.0, 36.0, 48.0]);
        // La note décalée reste trouvée même si la fenêtre commence après son pas.
        assert_eq!(ticks(&seq, 30.0, 40.0), [36.0]);
    }

    fn pt(tick: u32, value: f32, curve: f32) -> AutoPoint {
        AutoPoint { tick, value, curve }
    }

    #[test]
    fn automation_interpolates_with_curves_and_edges() {
        let lane = AutomationLane {
            target: AutoTarget::Tempo,
            scope: AutoScope::Song,
            points: vec![pt(96, 1.0, 0.0), pt(192, 3.0, 0.5), pt(288, 1.0, -0.5), pt(384, 5.0, 0.0)],
        };
        let v = |t: f64| lane.value_at(t).unwrap();
        // Avant le premier point et après le dernier : valeurs tenues.
        assert_eq!(v(0.0), 1.0);
        assert_eq!(v(-5.0), 1.0);
        assert_eq!(v(1_000.0), 5.0);
        assert_eq!(v(96.0), 1.0);
        assert_eq!(v(192.0), 3.0);
        assert_eq!(v(384.0), 5.0);
        // Linéaire.
        assert!((v(144.0) - 2.0).abs() < 1e-6);
        // Tension positive : départ lent, la descente 3 → 1 reste au-dessus de la droite.
        let slow = 0.5f64.powf(5f64.sqrt()) as f32;
        assert!((v(240.0) - (3.0 - 2.0 * slow)).abs() < 1e-5);
        assert!(v(240.0) > 2.0);
        // Tension négative : départ rapide, la montée 1 → 5 passe au-dessus de la droite.
        let fast = 0.5f64.powf(1.0 / 5f64.sqrt()) as f32;
        assert!((v(336.0) - (1.0 + 4.0 * fast)).abs() < 1e-5);
        assert!(v(336.0) > 3.0);
        // Monotone sur un segment courbé.
        let mut last = v(288.0);
        for t in 289..=384 {
            let x = v(t as f64);
            assert!(x >= last - 1e-6);
            last = x;
        }
        assert_eq!(shape_curve(0.0, 1.0), 0.0);
        assert_eq!(shape_curve(1.0, -1.0), 1.0);
        assert!((shape_curve(0.5, 1.0) - 0.5f64.powi(5)).abs() < 1e-12);

        let empty = AutomationLane { points: Vec::new(), ..lane.clone() };
        assert_eq!(empty.value_at(10.0), None);
        let single = AutomationLane { points: vec![pt(50, 0.3, 0.9)], ..lane };
        assert_eq!(single.value_at(0.0), Some(0.3));
        assert_eq!(single.value_at(99.0), Some(0.3));
    }

    #[test]
    fn automation_scopes() {
        let mut seq = Sequence {
            patterns: vec![pattern(&[], 96), pattern(&[], 192)],
            clips: vec![
                Clip { pattern: 1, start: 384, len: 384, offset: 48 },
                Clip { pattern: 0, start: 0, len: 96, offset: 0 },
            ],
            mode: PlayMode::Pattern(1),
            ..Default::default()
        };
        assert_eq!(seq.automation_tick(AutoScope::Song, 10.0), None);
        assert_eq!(seq.automation_tick(AutoScope::Pattern(0), 10.0), None);
        assert_eq!(seq.automation_tick(AutoScope::Pattern(1), 200.0), Some(8.0));
        seq.mode = PlayMode::Song;
        assert_eq!(seq.automation_tick(AutoScope::Song, 500.0), Some(500.0));
        // Relatif au clip, décalage compris, et en boucle dans le clip.
        assert_eq!(seq.automation_tick(AutoScope::Pattern(1), 384.0), Some(48.0));
        assert_eq!(seq.automation_tick(AutoScope::Pattern(1), 384.0 + 150.0), Some(6.0));
        assert_eq!(seq.automation_tick(AutoScope::Pattern(1), 300.0), None, "hors clip");
        assert_eq!(seq.automation_tick(AutoScope::Pattern(0), 50.0), Some(50.0));
        assert_eq!(seq.automation_tick(AutoScope::Pattern(7), 50.0), None);
    }
}
