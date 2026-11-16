//! Description des paramètres d'instruments et d'effets : l'interface construit
//! ses contrôles et la sauvegarde ses valeurs à partir de ces fiches.

use ponzik_i18n::{tr, tr_fmt};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    None,
    Hz,
    Db,
    Percent,
    Seconds,
    Semitones,
    Cents,
    Ratio,
    Pan,
    /// Liste de choix : la valeur est l'indice.
    Choice,
}

#[derive(Clone, Copy, Debug)]
pub struct ParamInfo {
    pub id: u32,
    pub name: &'static str,
    /// Section dans l'éditeur (« Oscillateurs », « Filtre »…).
    pub group: &'static str,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub unit: Unit,
    /// Course logarithmique (fréquences, temps).
    pub log: bool,
    pub choices: &'static [&'static str],
}

impl ParamInfo {
    pub const fn new(id: u32, name: &'static str, min: f32, max: f32, default: f32, unit: Unit) -> Self {
        Self { id, name, group: "", min, max, default, unit, log: false, choices: &[] }
    }

    pub const fn choice(id: u32, name: &'static str, choices: &'static [&'static str], default: usize) -> Self {
        Self {
            id,
            name,
            group: "",
            min: 0.0,
            max: (choices.len() - 1) as f32,
            default: default as f32,
            unit: Unit::Choice,
            log: false,
            choices,
        }
    }

    pub const fn log(mut self) -> Self {
        self.log = true;
        self
    }

    pub const fn group(mut self, group: &'static str) -> Self {
        self.group = group;
        self
    }

    pub fn clamp(&self, v: f32) -> f32 {
        let v = v.clamp(self.min, self.max);
        if self.unit == Unit::Choice { v.round() } else { v }
    }

    /// Valeur → position de contrôle dans [0, 1].
    pub fn to_norm(&self, v: f32) -> f32 {
        let v = self.clamp(v);
        if self.max <= self.min {
            return 0.0;
        }
        if self.log && self.min > 0.0 {
            (v / self.min).ln() / (self.max / self.min).ln()
        } else {
            (v - self.min) / (self.max - self.min)
        }
    }

    /// Position de contrôle dans [0, 1] → valeur.
    pub fn from_norm(&self, n: f32) -> f32 {
        let n = n.clamp(0.0, 1.0);
        let v = if self.log && self.min > 0.0 {
            self.min * (self.max / self.min).powf(n)
        } else {
            self.min + n * (self.max - self.min)
        };
        self.clamp(v)
    }

    /// Texte affiché dans la langue de l'interface : séparateur décimal de la
    /// langue, signe moins typographique, choix traduits.
    pub fn format(&self, v: f32) -> String {
        let num = |v: f32, decimals: usize| {
            let s = ponzik_i18n::num(v.abs() as f64, decimals);
            if v < 0.0 && s.chars().any(|c| c.is_ascii_digit() && c != '0') { format!("−{s}") } else { s }
        };
        match self.unit {
            Unit::Choice => tr(self.choices.get(v.round() as usize).copied().unwrap_or("?")).to_string(),
            Unit::Hz if v >= 1000.0 => format!("{} kHz", num(v / 1000.0, if v >= 10_000.0 { 1 } else { 2 })),
            Unit::Hz => format!("{} Hz", num(v, if v < 100.0 { 1 } else { 0 })),
            Unit::Db if v <= -79.9 => "−∞ dB".into(),
            Unit::Db => format!("{} dB", num(v, 1)),
            Unit::Percent => tr_fmt!("{}%", num(v * 100.0, 0)),
            Unit::Seconds if v < 1.0 => format!("{} ms", num(v * 1000.0, if v < 0.01 { 1 } else { 0 })),
            Unit::Seconds => format!("{} s", num(v, 2)),
            Unit::Semitones => format!("{}{} st", if v > 0.0 { "+" } else { "" }, num(v, 0)),
            Unit::Cents => format!("{}{} ct", if v > 0.0 { "+" } else { "" }, num(v, 0)),
            Unit::Ratio => format!("{}:1", num(v, 1)),
            Unit::Pan => {
                let p = (v * 100.0).round() as i32;
                match p {
                    0 => "C".into(),
                    p if p < 0 => tr_fmt!("L {}", -p),
                    p => tr_fmt!("R {}", p),
                }
            }
            Unit::None => num(v, 2),
        }
    }
}

/// Paramètres exposés par un instrument ou un effet.
pub trait Params {
    fn params(&self) -> &'static [ParamInfo] {
        &[]
    }

    fn get_param(&self, _id: u32) -> f32 {
        0.0
    }

    /// Appelé depuis le thread audio : ne doit ni allouer ni bloquer.
    fn set_param(&mut self, _id: u32, _value: f32) {}

    fn param_info(&self, id: u32) -> Option<&'static ParamInfo> {
        self.params().iter().find(|p| p.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalisation_roundtrip() {
        let lin = ParamInfo::new(0, "Gain", -24.0, 24.0, 0.0, Unit::Db);
        let log = ParamInfo::new(1, "Cutoff", 20.0, 20_000.0, 1_000.0, Unit::Hz).log();
        for n in [0.0, 0.25, 0.5, 1.0] {
            assert!((lin.to_norm(lin.from_norm(n)) - n).abs() < 1e-5);
            assert!((log.to_norm(log.from_norm(n)) - n).abs() < 1e-4);
        }
        assert!((log.from_norm(0.5) - 632.45).abs() < 0.5);
    }

    #[test]
    fn formatting() {
        let hz = ParamInfo::new(0, "f", 20.0, 20_000.0, 1_000.0, Unit::Hz);
        let db = ParamInfo::new(0, "g", -80.0, 6.0, 0.0, Unit::Db);
        let s = ParamInfo::new(0, "t", 0.001, 10.0, 0.1, Unit::Seconds);
        let pc = ParamInfo::new(0, "m", 0.0, 1.0, 0.5, Unit::Percent);
        let pan = ParamInfo::new(0, "p", -1.0, 1.0, 0.0, Unit::Pan);
        let c = ParamInfo::choice(0, "Wave", &["Sine", "Saw"], 1);
        ponzik_i18n::with_lang(ponzik_i18n::Lang::EN, || {
            assert_eq!(hz.format(440.0), "440 Hz");
            assert_eq!(hz.format(2_500.0), "2.50 kHz");
            assert_eq!(db.format(-3.04), "−3.0 dB");
            assert_eq!(db.format(-80.0), "−∞ dB");
            assert_eq!(s.format(0.25), "250 ms");
            assert_eq!(s.format(1.5), "1.50 s");
            assert_eq!(pc.format(0.5), "50%");
            assert_eq!(pan.format(-0.2), "L 20");
            assert_eq!(c.format(1.2), "Saw");
        });
        let fr = ponzik_i18n::Lang::from_code("fr").expect("fr");
        ponzik_i18n::with_lang(fr, || {
            assert_eq!(hz.format(2_500.0), "2,50 kHz");
            assert_eq!(db.format(-3.04), "−3,0 dB");
            assert_eq!(s.format(1.5), "1,50 s");
            assert_eq!(pc.format(0.5), "50 %");
            assert_eq!(pan.format(-0.2), "G 20");
            assert_eq!(pan.format(0.3), "D 30");
            assert_eq!(c.format(1.2), "Scie");
        });
        assert_eq!(c.clamp(0.7), 1.0);
    }
}
