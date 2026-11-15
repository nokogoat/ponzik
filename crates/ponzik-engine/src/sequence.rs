//! Données de séquence : notes, patterns, clips de playlist. Le moteur en reçoit
//! un instantané complet à chaque modification et y cherche les notes à jouer.

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

#[derive(Clone, Debug, PartialEq)]
pub struct Sequence {
    pub patterns: Vec<Pattern>,
    pub clips: Vec<Clip>,
    pub mode: PlayMode,
    /// Fin de la playlist en ticks (le morceau boucle ensuite).
    pub song_len: u32,
    /// Retard des doubles-croches paires, dans [0, 1] (1 = un demi-pas).
    pub swing: f32,
}

impl Default for Sequence {
    fn default() -> Self {
        Self { patterns: Vec::new(), clips: Vec::new(), mode: PlayMode::Pattern(0), song_len: TICKS_PER_BAR, swing: 0.0 }
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
    /// Fin de boucle du transport : la playlist boucle, un pattern est infini.
    pub fn loop_end(&self) -> Option<f64> {
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
            swing: 0.0,
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
}
