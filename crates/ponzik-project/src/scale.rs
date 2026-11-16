//! Gamme du projet (tonalité et mode) et accords.
//!
//! La tonalité est une classe de hauteur (0 = do … 11 = si) ; les notes sont des
//! numéros MIDI (60 = do 4). La gamme chromatique, par défaut, n'impose rien.

use crate::Project;

/// Noms des douze classes de hauteur, en notation française.
pub const KEY_NAMES: [&str; 12] = ["Do", "Do♯", "Ré", "Mi♭", "Mi", "Fa", "Fa♯", "Sol", "La♭", "La", "Si♭", "Si"];

/// Nom d'une tonalité (0–11, ramenée modulo 12).
pub fn key_name(key: u8) -> &'static str {
    KEY_NAMES[usize::from(key % 12)]
}

/// Mode de la gamme du projet.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ScaleKind {
    Major,
    Minor,
    Dorian,
    Phrygian,
    Lydian,
    Mixolydian,
    HarmonicMinor,
    MelodicMinor,
    PentatonicMajor,
    PentatonicMinor,
    Blues,
    /// Les douze notes : aucune gamme imposée.
    #[default]
    Chromatic,
}

impl ScaleKind {
    pub const ALL: [ScaleKind; 12] = [
        Self::Major,
        Self::Minor,
        Self::Dorian,
        Self::Phrygian,
        Self::Lydian,
        Self::Mixolydian,
        Self::HarmonicMinor,
        Self::MelodicMinor,
        Self::PentatonicMajor,
        Self::PentatonicMinor,
        Self::Blues,
        Self::Chromatic,
    ];

    /// Intervalles depuis la tonique, en demi-tons, croissants.
    pub fn intervals(self) -> &'static [u8] {
        match self {
            Self::Major => &[0, 2, 4, 5, 7, 9, 11],
            Self::Minor => &[0, 2, 3, 5, 7, 8, 10],
            Self::Dorian => &[0, 2, 3, 5, 7, 9, 10],
            Self::Phrygian => &[0, 1, 3, 5, 7, 8, 10],
            Self::Lydian => &[0, 2, 4, 6, 7, 9, 11],
            Self::Mixolydian => &[0, 2, 4, 5, 7, 9, 10],
            Self::HarmonicMinor => &[0, 2, 3, 5, 7, 8, 11],
            Self::MelodicMinor => &[0, 2, 3, 5, 7, 9, 11],
            Self::PentatonicMajor => &[0, 2, 4, 7, 9],
            Self::PentatonicMinor => &[0, 3, 5, 7, 10],
            Self::Blues => &[0, 3, 5, 6, 7, 10],
            Self::Chromatic => &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
        }
    }

    /// Nom affiché.
    pub fn name(self) -> &'static str {
        match self {
            Self::Major => "Majeur",
            Self::Minor => "Mineur",
            Self::Dorian => "Dorien",
            Self::Phrygian => "Phrygien",
            Self::Lydian => "Lydien",
            Self::Mixolydian => "Mixolydien",
            Self::HarmonicMinor => "Mineur harmonique",
            Self::MelodicMinor => "Mineur mélodique",
            Self::PentatonicMajor => "Pentatonique majeure",
            Self::PentatonicMinor => "Pentatonique mineure",
            Self::Blues => "Blues",
            Self::Chromatic => "Chromatique",
        }
    }

    /// Identifiant stable du fichier de projet.
    pub fn id(self) -> &'static str {
        match self {
            Self::Major => "major",
            Self::Minor => "minor",
            Self::Dorian => "dorian",
            Self::Phrygian => "phrygian",
            Self::Lydian => "lydian",
            Self::Mixolydian => "mixolydian",
            Self::HarmonicMinor => "harmonic_minor",
            Self::MelodicMinor => "melodic_minor",
            Self::PentatonicMajor => "pentatonic_major",
            Self::PentatonicMinor => "pentatonic_minor",
            Self::Blues => "blues",
            Self::Chromatic => "chromatic",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.id() == id)
    }

    /// Vrai si la note MIDI `note` appartient à la gamme de tonique `key`.
    pub fn contains(self, key: u8, note: u8) -> bool {
        let pc = (note % 12 + 12 - key % 12) % 12;
        self.intervals().contains(&pc)
    }

    /// Note de la gamme la plus proche de `note` (vers le bas à égalité).
    pub fn snap(self, key: u8, note: u8) -> u8 {
        let note = note.min(127);
        (0..12)
            .flat_map(|d| [note.checked_sub(d), note.checked_add(d).filter(|&n| n <= 127)])
            .flatten()
            .find(|&n| self.contains(key, n))
            .unwrap_or(note)
    }

    /// Accord diatonique : `size` notes empilées une sur deux dans la gamme, à
    /// partir du degré `degree` (0 = tonique ; au-delà de la gamme, on monte
    /// d'octave). La tonique est jouée dans l'octave `octave` (do 4 = 60) ; les
    /// notes hors de 0–127 sont omises.
    pub fn degree_chord(self, key: u8, degree: usize, size: usize, octave: i32) -> Vec<u8> {
        let steps = self.intervals();
        let tonic = 12 * (octave + 1) + i32::from(key % 12);
        (0..size)
            .map(|k| degree + 2 * k)
            .map(|i| tonic + 12 * (i / steps.len()) as i32 + i32::from(steps[i % steps.len()]))
            .filter_map(|n| u8::try_from(n).ok().filter(|&n| n <= 127))
            .collect()
    }
}

/// Type d'accord, défini par ses intervalles depuis la fondamentale.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChordKind {
    Major,
    Minor,
    Diminished,
    Augmented,
    Sus2,
    Sus4,
    /// Quinte à vide (fondamentale, quinte, octave).
    Power,
    Major7,
    Minor7,
    Dominant7,
    HalfDiminished7,
    Diminished7,
    MinorMajor7,
    Add9,
    MinorAdd9,
}

impl ChordKind {
    pub const ALL: [ChordKind; 15] = [
        Self::Major,
        Self::Minor,
        Self::Diminished,
        Self::Augmented,
        Self::Sus2,
        Self::Sus4,
        Self::Power,
        Self::Major7,
        Self::Minor7,
        Self::Dominant7,
        Self::HalfDiminished7,
        Self::Diminished7,
        Self::MinorMajor7,
        Self::Add9,
        Self::MinorAdd9,
    ];

    pub fn intervals(self) -> &'static [u8] {
        match self {
            Self::Major => &[0, 4, 7],
            Self::Minor => &[0, 3, 7],
            Self::Diminished => &[0, 3, 6],
            Self::Augmented => &[0, 4, 8],
            Self::Sus2 => &[0, 2, 7],
            Self::Sus4 => &[0, 5, 7],
            Self::Power => &[0, 7, 12],
            Self::Major7 => &[0, 4, 7, 11],
            Self::Minor7 => &[0, 3, 7, 10],
            Self::Dominant7 => &[0, 4, 7, 10],
            Self::HalfDiminished7 => &[0, 3, 6, 10],
            Self::Diminished7 => &[0, 3, 6, 9],
            Self::MinorMajor7 => &[0, 3, 7, 11],
            Self::Add9 => &[0, 4, 7, 14],
            Self::MinorAdd9 => &[0, 3, 7, 14],
        }
    }

    /// Suffixe usuel (« m7 », « sus4 »…), vide pour l'accord majeur.
    pub fn symbol(self) -> &'static str {
        match self {
            Self::Major => "",
            Self::Minor => "m",
            Self::Diminished => "dim",
            Self::Augmented => "aug",
            Self::Sus2 => "sus2",
            Self::Sus4 => "sus4",
            Self::Power => "5",
            Self::Major7 => "maj7",
            Self::Minor7 => "m7",
            Self::Dominant7 => "7",
            Self::HalfDiminished7 => "m7♭5",
            Self::Diminished7 => "dim7",
            Self::MinorMajor7 => "m(maj7)",
            Self::Add9 => "add9",
            Self::MinorAdd9 => "madd9",
        }
    }

    /// Reconnaît un accord à partir de ses notes (la première est la fondamentale).
    pub fn identify(notes: &[u8]) -> Option<Self> {
        let root = *notes.first()?;
        let intervals: Vec<u8> = notes.iter().map(|&n| n.checked_sub(root)).collect::<Option<_>>()?;
        Self::ALL.into_iter().find(|k| k.intervals() == intervals.as_slice())
    }
}

/// Notes MIDI d'un accord posé sur `root` ; celles au-dessus de 127 sont omises.
pub fn chord(root: u8, kind: ChordKind) -> Vec<u8> {
    kind.intervals().iter().map(|&i| u16::from(root) + u16::from(i)).filter(|&n| n <= 127).map(|n| n as u8).collect()
}

impl Project {
    /// Classes de hauteur (0–11) de la gamme du projet, dans l'ordre depuis la tonique.
    pub fn scale_notes(&self) -> Vec<u8> {
        self.scale.intervals().iter().map(|&i| (self.key % 12 + i) % 12).collect()
    }

    /// Vrai si la note MIDI appartient à la gamme du projet.
    pub fn in_scale(&self, note: u8) -> bool {
        self.scale.contains(self.key, note)
    }

    /// Note de la gamme du projet la plus proche (« coller à la gamme »).
    pub fn snap(&self, note: u8) -> u8 {
        self.scale.snap(self.key, note)
    }

    /// Accord diatonique du projet, voir [`ScaleKind::degree_chord`].
    pub fn diatonic_chord(&self, degree: usize, size: usize, octave: i32) -> Vec<u8> {
        self.scale.degree_chord(self.key, degree, size, octave)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(key: u8, scale: ScaleKind) -> Project {
        Project { key, scale, ..Project::empty() }
    }

    #[test]
    fn scales_are_sorted_and_named() {
        for s in ScaleKind::ALL {
            let iv = s.intervals();
            assert_eq!(iv[0], 0);
            assert!(iv.windows(2).all(|w| w[0] < w[1]) && iv[iv.len() - 1] < 12, "{s:?}");
            assert_eq!(ScaleKind::from_id(s.id()), Some(s));
            assert!(!s.name().is_empty());
        }
        assert_eq!(ScaleKind::from_id("inconnue"), None);
        assert_eq!(ScaleKind::default(), ScaleKind::Chromatic);
        assert_eq!(key_name(9), "La");
        assert_eq!(key_name(21), "La");
    }

    #[test]
    fn scale_membership_and_snap() {
        let a_minor = project(9, ScaleKind::Minor);
        assert_eq!(a_minor.scale_notes(), [9, 11, 0, 2, 4, 5, 7]);
        assert!(a_minor.in_scale(57) && a_minor.in_scale(60) && !a_minor.in_scale(61));
        // Do♯ 4 → do 4 (égalité : vers le bas), fa♯ → fa, sol♯ → sol.
        assert_eq!(a_minor.snap(61), 60);
        assert_eq!(a_minor.snap(66), 65);
        assert_eq!(a_minor.snap(68), 67);
        assert_eq!(a_minor.snap(69), 69);

        let c_penta = project(0, ScaleKind::PentatonicMajor);
        assert_eq!(c_penta.snap(65), 64, "fa → mi");
        assert_eq!(c_penta.snap(66), 67, "fa♯ → sol");
        // Bornes MIDI.
        assert_eq!(project(1, ScaleKind::Major).snap(127), 126);
        assert_eq!(project(2, ScaleKind::Major).snap(0), 1);

        let chromatic = Project::empty();
        assert!((0..=127).all(|n| chromatic.in_scale(n) && chromatic.snap(n) == n));
    }

    #[test]
    fn chords() {
        assert_eq!(chord(60, ChordKind::Major), [60, 64, 67]);
        assert_eq!(chord(57, ChordKind::Minor7), [57, 60, 64, 67]);
        assert_eq!(chord(60, ChordKind::Sus4), [60, 65, 67]);
        assert_eq!(chord(60, ChordKind::Add9), [60, 64, 67, 74]);
        assert_eq!(chord(120, ChordKind::Major7), [120, 124, 127]);
        for k in ChordKind::ALL {
            assert_eq!(ChordKind::identify(&chord(40, k)), Some(k));
        }
        assert_eq!(ChordKind::identify(&[60, 61]), None);
        assert_eq!(ChordKind::identify(&[]), None);
    }

    #[test]
    fn diatonic_chords() {
        let c = project(0, ScaleKind::Major);
        let quality = |d, size| ChordKind::identify(&c.diatonic_chord(d, size, 4)).unwrap();
        assert_eq!(c.diatonic_chord(0, 3, 4), [60, 64, 67]);
        assert_eq!(c.diatonic_chord(4, 4, 4), [67, 71, 74, 77]);
        let triads: Vec<ChordKind> = (0..7).map(|d| quality(d, 3)).collect();
        use ChordKind::*;
        assert_eq!(triads, [Major, Minor, Minor, Major, Major, Minor, Diminished]);
        let sevenths: Vec<ChordKind> = (0..7).map(|d| quality(d, 4)).collect();
        assert_eq!(sevenths, [Major7, Minor7, Minor7, Major7, Dominant7, Minor7, HalfDiminished7]);

        // La mineur harmonique : le Ve degré est majeur (mi sol♯ si).
        let a = project(9, ScaleKind::HarmonicMinor);
        assert_eq!(a.diatonic_chord(4, 3, 3), [64, 68, 71]);
        // Au-delà de la gamme, on monte d'octave ; hors MIDI, omis.
        assert_eq!(c.diatonic_chord(7, 1, 4), [72]);
        assert_eq!(c.diatonic_chord(0, 3, 9), [120, 124, 127]);
        assert!(c.diatonic_chord(0, 3, -2).is_empty());
    }
}
