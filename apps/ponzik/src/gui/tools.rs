//! MIDI tools of the piano roll: transformations of the selection (or the
//! whole channel) and generators, each in a small panel with its settings and
//! a live preview of the result. Applying is one undo step.

use ponzik_engine::{Note, NoteCond, ParamInfo, TICKS_PER_BAR, TICKS_PER_STEP, Unit};
use ponzik_i18n::{tr, tr_fmt, tr_fmt_n, tr_noop};
use ponzik_project::ScaleKind;
use ponzik_project::generate::{self, BassRhythm, Contour, MelodySpec, Mood, ProgressionSpec, RhythmStyle};
use ponzik_project::transform::{self, ArpDir};
use ponzik_ui::{Canvas, Color, Rect};
use ponzik_window::Key;

use super::Input;
use super::app::{Action, App, MenuItem, Popup};
use super::theme::*;
use super::widgets::{Icon, draw_icon, wid};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Arpeggiate,
    Strum,
    Flam,
    Legato,
    Staccato,
    Reverse,
    Invert,
    Stretch,
    Glue,
    Quantize,
    Humanize,
    ScaleCorrect,
    Euclid,
    Rhythm,
    Melody,
    Bassline,
    Chords,
}

impl Tool {
    pub const TRANSFORMS: [Tool; 12] = [
        Self::Arpeggiate,
        Self::Strum,
        Self::Flam,
        Self::Legato,
        Self::Staccato,
        Self::Reverse,
        Self::Invert,
        Self::Stretch,
        Self::Glue,
        Self::Quantize,
        Self::Humanize,
        Self::ScaleCorrect,
    ];
    pub const GENERATORS: [Tool; 5] = [Self::Euclid, Self::Rhythm, Self::Melody, Self::Bassline, Self::Chords];

    pub fn generator(self) -> bool {
        Self::GENERATORS.contains(&self)
    }

    /// Name (to translate).
    pub fn name(self) -> &'static str {
        match self {
            Self::Arpeggiate => tr_noop("Arpeggiate"),
            Self::Strum => tr_noop("Strum"),
            Self::Flam => tr_noop("Flam"),
            Self::Legato => tr_noop("Legato"),
            Self::Staccato => tr_noop("Staccato"),
            Self::Reverse => tr_noop("Reverse"),
            Self::Invert => tr_noop("Invert"),
            Self::Stretch => tr_noop("Stretch time"),
            Self::Glue => tr_noop("Glue"),
            Self::Quantize => tr_noop("Quantize"),
            Self::Humanize => tr_noop("Humanize"),
            Self::ScaleCorrect => tr_noop("Fit to scale"),
            Self::Euclid => tr_noop("Euclidean rhythm"),
            Self::Rhythm => tr_noop("Drum rhythm"),
            Self::Melody => tr_noop("Melody"),
            Self::Bassline => tr_noop("Bassline"),
            Self::Chords => tr_noop("Chord progression"),
        }
    }

    /// What it does, in one line (to translate).
    pub fn tip(self) -> &'static str {
        match self {
            Self::Arpeggiate => tr_noop("Chords become arpeggios"),
            Self::Strum => tr_noop("Spreads each chord like a guitar strum"),
            Self::Flam => tr_noop("A soft grace note just before each note"),
            Self::Legato => tr_noop("Each note lasts until the next one"),
            Self::Staccato => tr_noop("Shortens the notes"),
            Self::Reverse => tr_noop("Plays the notes backwards"),
            Self::Invert => tr_noop("Mirrors the pitches: up becomes down"),
            Self::Stretch => tr_noop("Twice as slow, twice as fast, or anything between"),
            Self::Glue => tr_noop("Merges overlapping notes of the same pitch"),
            Self::Quantize => tr_noop("Pulls the notes to the grid, with swing"),
            Self::Humanize => tr_noop("Random timing and velocity"),
            Self::ScaleCorrect => tr_noop("Moves wrong notes to the nearest note of the scale"),
            Self::Euclid => tr_noop("Pulses spread evenly over the steps"),
            Self::Rhythm => tr_noop("A drum part by style and density"),
            Self::Melody => tr_noop("A melody on the scale"),
            Self::Bassline => tr_noop("A bassline that follows the chords"),
            Self::Chords => tr_noop("A progression in the key, by mood"),
        }
    }

    pub fn icon(self) -> Icon {
        match self {
            Self::Arpeggiate => Icon::Arpeggio,
            Self::Strum => Icon::Strum,
            Self::Flam => Icon::Flam,
            Self::Legato => Icon::Legato,
            Self::Staccato => Icon::Staccato,
            Self::Reverse => Icon::Swap,
            Self::Invert => Icon::Invert,
            Self::Stretch => Icon::Stretch,
            Self::Glue => Icon::Glue,
            Self::Quantize => Icon::Magnet,
            Self::Humanize => Icon::Dice,
            Self::ScaleCorrect => Icon::Scale,
            Self::Euclid => Icon::Euclid,
            Self::Rhythm => Icon::Rack,
            Self::Melody => Icon::Melody,
            Self::Bassline => Icon::Bass,
            Self::Chords => Icon::Chord,
        }
    }

    /// Identifier for the screenshot hooks.
    pub fn from_id(id: &str) -> Option<Tool> {
        Self::TRANSFORMS.into_iter().chain(Self::GENERATORS).find(|t| format!("{t:?}").eq_ignore_ascii_case(id))
    }
}

/// Note values offered for rates and grids: label and ticks.
const RATES: [(&str, u32); 6] = [("1/4", 96), ("1/8", 48), ("1/16", 24), ("1/32", 12), ("1/8T", 32), ("1/16T", 16)];
const RATE_LABELS: [&str; 6] = ["1/4", "1/8", "1/16", "1/32", "1/8T", "1/16T"];
const SPREADS: [(&str, u32); 5] = [("1/64", 6), ("1/32", 12), ("1/16", 24), ("1/8", 48), ("1/4", 96)];
const SPREAD_LABELS: [&str; 5] = ["1/64", "1/32", "1/16", "1/8", "1/4"];
const FLAMS: [u32; 3] = [3, 6, 12];
const FLAM_LABELS: [&str; 3] = ["1/128", "1/64", "1/32"];
const CHORD_LENS: [u32; 3] = [TICKS_PER_BAR / 2, TICKS_PER_BAR, 2 * TICKS_PER_BAR];
const CHORD_LEN_LABELS: [&str; 3] = [tr_noop("Half bar"), tr_noop("1 bar"), tr_noop("2 bars")];
/// Small numbers, for the counts shown by the choice widget.
const NUMS: [&str; 33] = [
    "0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11", "12", "13", "14", "15", "16", "17", "18", "19", "20", "21", "22", "23", "24",
    "25", "26", "27", "28", "29", "30", "31", "32",
];

/// Conditions offered in the editors, in order.
pub const CONDS: [NoteCond; 20] = [
    NoteCond::Always,
    NoteCond::First,
    NoteCond::NotFirst,
    NoteCond::Every(1, 2),
    NoteCond::Every(2, 2),
    NoteCond::Every(1, 3),
    NoteCond::Every(2, 3),
    NoteCond::Every(3, 3),
    NoteCond::Every(1, 4),
    NoteCond::Every(2, 4),
    NoteCond::Every(3, 4),
    NoteCond::Every(4, 4),
    NoteCond::Every(1, 8),
    NoteCond::Every(2, 8),
    NoteCond::Every(3, 8),
    NoteCond::Every(4, 8),
    NoteCond::Every(5, 8),
    NoteCond::Every(6, 8),
    NoteCond::Every(7, 8),
    NoteCond::Every(8, 8),
];
pub const COND_LABELS: [&str; 20] = [
    tr_noop("Always"),
    tr_noop("1st pass"),
    tr_noop("Not 1st"),
    "1:2",
    "2:2",
    "1:3",
    "2:3",
    "3:3",
    "1:4",
    "2:4",
    "3:4",
    "4:4",
    "1:8",
    "2:8",
    "3:8",
    "4:8",
    "5:8",
    "6:8",
    "7:8",
    "8:8",
];

/// Short label of a condition, for lanes and pads.
pub fn cond_label(c: NoteCond) -> String {
    match c {
        NoteCond::Always => String::new(),
        NoteCond::First => tr("1st").into(),
        NoteCond::NotFirst => tr("!1st").into(),
        NoteCond::Every(n, m) => format!("{n}:{m}"),
    }
}

pub fn cond_index(c: NoteCond) -> usize {
    CONDS.iter().position(|&x| x == c).unwrap_or(0)
}

const ARP_DIR: ParamInfo = ParamInfo::choice(0, tr_noop("Direction"), &[tr_noop("Up"), tr_noop("Down"), tr_noop("Up-down"), tr_noop("Random")], 0);
const RATE: ParamInfo = ParamInfo::choice(0, tr_noop("Rate"), &RATE_LABELS, 2);
const GRID: ParamInfo = ParamInfo::choice(0, tr_noop("Grid"), &RATE_LABELS, 2);
const SPREAD: ParamInfo = ParamInfo::choice(0, tr_noop("Spread"), &SPREAD_LABELS, 1);
const STRUM_DIR: ParamInfo = ParamInfo::choice(0, tr_noop("Direction"), &[tr_noop("Up"), tr_noop("Down")], 0);
const FLAM_TIME: ParamInfo = ParamInfo::choice(0, tr_noop("Offset"), &FLAM_LABELS, 1);
const GRACE: ParamInfo = ParamInfo::new(0, tr_noop("Grace velocity"), 0.1, 1.0, 0.5, Unit::Percent);
const LENGTH: ParamInfo = ParamInfo::new(0, tr_noop("Length"), 0.05, 0.95, 0.5, Unit::Percent);
const INVERT_MODE: ParamInfo = ParamInfo::choice(0, tr_noop("Mirror"), &[tr_noop("Semitones"), tr_noop("Scale steps")], 1);
const CENTER: ParamInfo = ParamInfo::new(0, tr_noop("Center"), -12.0, 12.0, 0.0, Unit::Semitones);
const FACTOR: ParamInfo = ParamInfo::new(0, tr_noop("Factor"), 0.25, 4.0, 2.0, Unit::Percent).log();
const STRENGTH: ParamInfo = ParamInfo::new(0, tr_noop("Strength"), 0.0, 1.0, 1.0, Unit::Percent);
const SWING: ParamInfo = ParamInfo::new(0, tr_noop("Swing"), 0.0, 1.0, 0.0, Unit::Percent);
const TIMING: ParamInfo = ParamInfo::new(0, tr_noop("Timing"), 0.0, 1.0, 0.25, Unit::Percent);
const VELOCITY: ParamInfo = ParamInfo::new(0, tr_noop("Velocity"), 0.0, 0.5, 0.1, Unit::Percent);
const STEPS: ParamInfo = ParamInfo::choice(0, tr_noop("Steps"), &NUMS, 8);
const PULSES: ParamInfo = ParamInfo::choice(0, tr_noop("Pulses"), &NUMS, 3);
const ROTATE: ParamInfo = ParamInfo::choice(0, tr_noop("Rotate"), &NUMS, 0);
const STYLE: ParamInfo = ParamInfo::choice(
    0,
    tr_noop("Style"),
    &[tr_noop("Kick"), tr_noop("Snare"), tr_noop("Hats"), tr_noop("Trap hats"), tr_noop("Break"), tr_noop("Percussion")],
    0,
);
const DENSITY: ParamInfo = ParamInfo::new(0, tr_noop("Density"), 0.0, 1.0, 0.5, Unit::Percent);
const CONTOUR: ParamInfo = ParamInfo::choice(
    0,
    tr_noop("Contour"),
    &[tr_noop("Rising"), tr_noop("Falling"), tr_noop("Arch"), tr_noop("Valley"), tr_noop("Wave"), tr_noop("Free")],
    2,
);
const RANGE: ParamInfo = ParamInfo::new(0, tr_noop("Range"), 5.0, 24.0, 12.0, Unit::Semitones);
const OCTAVE: ParamInfo = ParamInfo::choice(0, tr_noop("Octave"), &NUMS, 4);
const MOOD: ParamInfo = ParamInfo::choice(
    0,
    tr_noop("Mood"),
    &[tr_noop("Uplifting"), tr_noop("Melancholic"), tr_noop("Epic"), tr_noop("Dreamy"), tr_noop("Dark"), tr_noop("Jazzy")],
    0,
);
const BASS_RHYTHM: ParamInfo = ParamInfo::choice(
    0,
    tr_noop("Rhythm"),
    &[tr_noop("Sustained"), tr_noop("Eighths"), tr_noop("Root-fifth"), tr_noop("Octaves"), tr_noop("Syncopated"), tr_noop("Walking")],
    1,
);
const CHORD_LEN: ParamInfo = ParamInfo::choice(0, tr_noop("Chord length"), &CHORD_LEN_LABELS, 1);

/// Settings of the tools, kept between uses.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolState {
    pub seed: u64,
    pub arp_dir: ArpDir,
    pub arp_rate: usize,
    pub strum_spread: usize,
    pub strum_up: bool,
    pub flam: usize,
    pub grace: f32,
    pub staccato: f32,
    pub invert_scale: bool,
    pub invert_center: i32,
    pub factor: f32,
    pub grid: usize,
    pub strength: f32,
    pub swing: f32,
    pub timing: f32,
    pub vel: f32,
    pub euclid: (usize, usize, usize),
    pub euclid_rate: usize,
    pub style: RhythmStyle,
    pub density: f32,
    pub contour: Contour,
    pub range: u8,
    pub mel_octave: i32,
    pub mood: Mood,
    pub bass_rhythm: BassRhythm,
    pub bass_octave: i32,
    /// Channel whose chords the bassline follows (`None`: the mood's progression).
    pub bass_source: Option<usize>,
    pub chord_len: usize,
    pub chord_octave: i32,
}

impl Default for ToolState {
    fn default() -> Self {
        Self {
            seed: 1,
            arp_dir: ArpDir::Up,
            arp_rate: 2,
            strum_spread: 1,
            strum_up: true,
            flam: 1,
            grace: 0.5,
            staccato: 0.5,
            invert_scale: true,
            invert_center: 0,
            factor: 2.0,
            grid: 2,
            strength: 1.0,
            swing: 0.0,
            timing: 0.25,
            vel: 0.1,
            euclid: (8, 3, 0),
            euclid_rate: 2,
            style: RhythmStyle::Kick,
            density: 0.5,
            contour: Contour::Arch,
            range: 12,
            mel_octave: 4,
            mood: Mood::Uplifting,
            bass_rhythm: BassRhythm::Eighths,
            bass_octave: 2,
            bass_source: None,
            chord_len: 1,
            chord_octave: 4,
        }
    }
}

/// What a tool would do: on channel `ch`, the notes at `replaced` (in
/// `before`) give way to `after`.
pub struct ToolResult {
    pub ch: usize,
    pub replaced: Vec<usize>,
    pub before: Vec<Note>,
    pub after: Vec<Note>,
}

fn pick<T: Copy + PartialEq>(all: &[T], v: T) -> usize {
    all.iter().position(|&x| x == v).unwrap_or(0)
}

impl App {
    /// The channel whose notes look most like chords (the most notes starting
    /// together), other than `ch`: what a bassline follows by default.
    fn chord_channel(&self, ch: usize) -> Option<usize> {
        let pat = &self.project.patterns[self.project.current_pattern];
        (0..self.project.channels.len())
            .filter(|&c| c != ch)
            .map(|c| {
                let notes = pat.notes.get(c).map_or(&[][..], |v| v.as_slice());
                let stacked = notes.iter().filter(|n| notes.iter().filter(|m| m.start == n.start).count() >= 2).count();
                (c, stacked)
            })
            .filter(|&(_, s)| s > 0)
            .max_by_key(|&(c, s)| (s, usize::MAX - c))
            .map(|(c, _)| c)
    }

    /// Channels with notes in the pattern, other than `ch`.
    fn channels_with_notes(&self, ch: usize) -> Vec<usize> {
        let pat = &self.project.patterns[self.project.current_pattern];
        (0..self.project.channels.len()).filter(|&c| c != ch && pat.notes.get(c).is_some_and(|v| !v.is_empty())).collect()
    }

    /// What `tool` would do now, or `None` with nothing to work on.
    pub(super) fn tool_result(&self, tool: Tool) -> Option<ToolResult> {
        let ch = self.piano_channel()?;
        let st = &self.piano.tool;
        let (key, scale) = (self.project.key, self.project.scale);
        let all: Vec<usize> = (0..self.notes(ch).len()).collect();
        let replaced = if tool.generator() || self.piano.selected.is_empty() { all } else { self.piano.selected.clone() };
        let before: Vec<Note> = replaced.iter().filter_map(|&i| self.notes(ch).get(i).copied()).collect();
        if !tool.generator() && before.is_empty() {
            return None;
        }
        let len = self.project.patterns[self.project.current_pattern].len.max(TICKS_PER_BAR);
        let root = self.project.channels[ch].root_note;
        let seed = st.seed;
        let after = match tool {
            Tool::Arpeggiate => transform::arpeggiate(&before, st.arp_dir, RATES[st.arp_rate].1, seed),
            Tool::Strum => transform::strum(&before, SPREADS[st.strum_spread].1, st.strum_up),
            Tool::Flam => transform::flam(&before, FLAMS[st.flam], st.grace),
            Tool::Legato => transform::legato(&before),
            Tool::Staccato => transform::staccato(&before, st.staccato),
            Tool::Reverse => transform::reverse(&before),
            Tool::Invert => {
                let center = (i32::from(transform::middle_key(&before)) + st.invert_center).clamp(0, 127) as u8;
                transform::invert(&before, center, st.invert_scale.then_some((key, scale)))
            }
            Tool::Stretch => transform::stretch(&before, st.factor),
            Tool::Glue => transform::glue(&before),
            Tool::Quantize => transform::quantize(&before, RATES[st.grid].1, st.strength, st.swing),
            Tool::Humanize => transform::humanize(&before, (st.timing * TICKS_PER_STEP as f32).round() as u32, st.vel, seed),
            Tool::ScaleCorrect => transform::scale_correct(&before, key, scale),
            Tool::Euclid => {
                let (steps, pulses, rot) = st.euclid;
                generate::euclid_notes(len, RATES[st.euclid_rate].1, steps, pulses, rot, root, 0.9)
            }
            Tool::Rhythm => generate::rhythm(len, st.style, st.density, root, seed),
            Tool::Melody => {
                let low = (12 * (st.mel_octave + 1) + i32::from(key % 12)).clamp(0, 120) as u8;
                generate::melody(len, MelodySpec { key, scale, low, range: st.range, density: st.density, contour: st.contour }, seed)
            }
            Tool::Bassline => {
                let chord_len = CHORD_LENS[st.chord_len];
                let source = st.bass_source.filter(|&c| c != ch && c < self.project.channels.len());
                let chords = match source {
                    Some(c) => generate::chords_from_notes(self.notes(c), len),
                    None => generate::chords_from_degrees(&generate::progression_degrees(st.mood, seed), key, scale, chord_len, len),
                };
                generate::bassline(&chords, st.bass_octave, st.bass_rhythm, seed)
            }
            Tool::Chords => generate::progression(
                len,
                ProgressionSpec { key, scale, mood: st.mood, chord_len: CHORD_LENS[st.chord_len], octave: st.chord_octave },
                seed,
            ),
        };
        Some(ToolResult { ch, replaced, before, after })
    }

    /// Opens a tool's panel (a generator following chords looks for them first).
    pub(super) fn open_tool(&mut self, tool: Tool, anchor: Rect) {
        if tool == Tool::Bassline
            && let Some(ch) = self.piano_channel()
            && self.piano.tool.bass_source.is_none_or(|c| c == ch || c >= self.project.channels.len())
        {
            self.piano.tool.bass_source = self.chord_channel(ch);
        }
        self.open_popup(Popup::Tool(tool), anchor);
    }

    /// Applies `tool`: one undo step.
    pub(super) fn apply_tool(&mut self, tool: Tool) {
        let Some(r) = self.tool_result(tool) else {
            self.toast(tr("No notes to transform"));
            return;
        };
        let mut gone = r.replaced.clone();
        gone.sort_unstable();
        let count = r.after.len();
        self.break_undo_merge();
        let notes = self.notes_mut(r.ch);
        for i in gone.into_iter().rev() {
            if i < notes.len() {
                notes.remove(i);
            }
        }
        let base = notes.len();
        notes.extend(r.after);
        self.piano.selected = if tool.generator() { Vec::new() } else { (base..base + count).collect() };
        self.finish_note_edit(r.ch, tr(tool.name()));
        self.break_undo_merge();
        self.toast(&if tool.generator() {
            tr_fmt_n!("{} note generated", "{} notes generated", count, count)
        } else {
            tr_fmt!("{} · {}", tr(tool.name()), tr_fmt_n!("{} note", "{} notes", count, count))
        });
    }

    /// The tools menu: transformations, then generators.
    pub(super) fn tools_menu(&self) -> (String, Vec<MenuItem>) {
        let mut v: Vec<MenuItem> = Tool::TRANSFORMS
            .into_iter()
            .map(|t| MenuItem::new(t.icon(), tr(t.name()), Action::OpenTool(t)).tip(tr(t.tip())))
            .collect();
        for (i, t) in Tool::GENERATORS.into_iter().enumerate() {
            let it = MenuItem::new(t.icon(), tr(t.name()), Action::OpenTool(t)).tip(tr(t.tip())).detail(tr("generate").into());
            v.push(if i == 0 { it.sep() } else { it });
        }
        let title = if self.piano.selected.is_empty() {
            tr("MIDI tools · the whole channel").to_string()
        } else {
            tr_fmt_n!("MIDI tools · {} selected note", "MIDI tools · {} selected notes", self.piano.selected.len(), self.piano.selected.len())
        };
        (title, v)
    }

    /// A choice control (enum or count); returns the new index.
    fn tool_choice(&mut self, c: &mut Canvas, input: &Input, n: usize, cell: Rect, info: &ParamInfo, index: usize) -> Option<usize> {
        self.choice(c, input, wid("tool-ctl", n, 0), cell, info, index as f32).map(|v| v.round().max(0.0) as usize)
    }

    fn tool_knob(&mut self, c: &mut Canvas, input: &Input, n: usize, cell: Rect, info: &ParamInfo, value: f32) -> Option<f32> {
        self.knob(c, input, wid("tool-ctl", n, 0), cell, info, value)
    }

    /// Draws the settings of `tool` in `area`, one cell after the other.
    fn tool_controls(&mut self, c: &mut Canvas, input: &Input, tool: Tool, area: Rect) {
        const CELL: f32 = 96.0;
        let cell = |i: usize| Rect::new(area.x + i as f32 * (CELL + 8.0), area.y, CELL, area.h);
        let st = self.piano.tool.clone();
        let mut s = st.clone();
        match tool {
            Tool::Arpeggiate => {
                if let Some(i) = self.tool_choice(c, input, 0, cell(0), &ARP_DIR, pick(&ArpDir::ALL, st.arp_dir)) {
                    s.arp_dir = ArpDir::ALL[i.min(3)];
                }
                if let Some(i) = self.tool_choice(c, input, 1, cell(1), &RATE, st.arp_rate) {
                    s.arp_rate = i.min(RATES.len() - 1);
                }
            }
            Tool::Strum => {
                if let Some(i) = self.tool_choice(c, input, 0, cell(0), &SPREAD, st.strum_spread) {
                    s.strum_spread = i.min(SPREADS.len() - 1);
                }
                if let Some(i) = self.tool_choice(c, input, 1, cell(1), &STRUM_DIR, usize::from(!st.strum_up)) {
                    s.strum_up = i == 0;
                }
            }
            Tool::Flam => {
                if let Some(i) = self.tool_choice(c, input, 0, cell(0), &FLAM_TIME, st.flam) {
                    s.flam = i.min(FLAMS.len() - 1);
                }
                if let Some(v) = self.tool_knob(c, input, 1, cell(1), &GRACE, st.grace) {
                    s.grace = v;
                }
            }
            Tool::Staccato => {
                if let Some(v) = self.tool_knob(c, input, 0, cell(0), &LENGTH, st.staccato) {
                    s.staccato = v;
                }
            }
            Tool::Invert => {
                if let Some(i) = self.tool_choice(c, input, 0, cell(0), &INVERT_MODE, usize::from(st.invert_scale)) {
                    s.invert_scale = i == 1;
                }
                if let Some(v) = self.tool_knob(c, input, 1, cell(1), &CENTER, st.invert_center as f32) {
                    s.invert_center = v.round() as i32;
                }
            }
            Tool::Stretch => {
                if let Some(v) = self.tool_knob(c, input, 0, cell(0), &FACTOR, st.factor) {
                    s.factor = v;
                }
                // Quick picks: ÷2, ×2, ÷4, ×4.
                for (k, (label, f)) in [("÷2", 0.5), ("×2", 2.0), ("÷4", 0.25), ("×4", 4.0)].into_iter().enumerate() {
                    let r = Rect::new(cell(1).x + (k % 2) as f32 * 52.0, area.y + 6.0 + (k / 2) as f32 * 32.0, 46.0, 26.0);
                    let id = wid("tool-quick", k, 0);
                    let hov = self.hovered(input, id, r);
                    let on = (st.factor - f).abs() < 1e-3;
                    self.choice_chip(c, r, id, hov, on, label);
                    if hov && input.pressed {
                        s.factor = f;
                    }
                }
            }
            Tool::Quantize => {
                if let Some(i) = self.tool_choice(c, input, 0, cell(0), &GRID, st.grid) {
                    s.grid = i.min(RATES.len() - 1);
                }
                if let Some(v) = self.tool_knob(c, input, 1, cell(1), &STRENGTH, st.strength) {
                    s.strength = v;
                }
                if let Some(v) = self.tool_knob(c, input, 2, cell(2), &SWING, st.swing) {
                    s.swing = v;
                }
            }
            Tool::Humanize => {
                if let Some(v) = self.tool_knob(c, input, 0, cell(0), &TIMING, st.timing) {
                    s.timing = v;
                }
                if let Some(v) = self.tool_knob(c, input, 1, cell(1), &VELOCITY, st.vel) {
                    s.vel = v;
                }
            }
            Tool::Legato | Tool::Reverse | Tool::Glue | Tool::ScaleCorrect => {
                let text = match tool {
                    Tool::ScaleCorrect if self.project.scale == ScaleKind::Chromatic => tr("Pick a scale first, at the top").to_string(),
                    Tool::ScaleCorrect => {
                        tr_fmt!("Scale: {} {}", super::app::key_name(self.project.key), tr(self.project.scale.name()).to_lowercase())
                    }
                    _ => tr("No settings: what you see is what you get").to_string(),
                };
                let t = self.theme;
                self.text(c, self.f.sans, 12.0, area.x + 4.0, area.y + 18.0, &text, t.muted);
            }
            Tool::Euclid => {
                let (steps, pulses, rot) = st.euclid;
                let steps_info = ParamInfo { min: 0.0, max: 32.0, ..STEPS };
                if let Some(i) = self.tool_choice(c, input, 0, cell(0), &steps_info, steps) {
                    let n = i.clamp(1, 32);
                    s.euclid = (n, pulses.min(n), rot.min(n - 1));
                }
                let pulses_info = ParamInfo { max: steps as f32, ..PULSES };
                if let Some(i) = self.tool_choice(c, input, 1, cell(1), &pulses_info, pulses) {
                    s.euclid.1 = i.min(steps);
                }
                let rot_info = ParamInfo { max: steps.saturating_sub(1) as f32, ..ROTATE };
                if let Some(i) = self.tool_choice(c, input, 2, cell(2), &rot_info, rot) {
                    s.euclid.2 = i.min(steps.saturating_sub(1));
                }
                if let Some(i) = self.tool_choice(c, input, 3, cell(3), &RATE, st.euclid_rate) {
                    s.euclid_rate = i.min(RATES.len() - 1);
                }
            }
            Tool::Rhythm => {
                if let Some(i) = self.tool_choice(c, input, 0, cell(0), &STYLE, pick(&RhythmStyle::ALL, st.style)) {
                    s.style = RhythmStyle::ALL[i.min(RhythmStyle::ALL.len() - 1)];
                }
                if let Some(v) = self.tool_knob(c, input, 1, cell(1), &DENSITY, st.density) {
                    s.density = v;
                }
            }
            Tool::Melody => {
                if let Some(i) = self.tool_choice(c, input, 0, cell(0), &CONTOUR, pick(&Contour::ALL, st.contour)) {
                    s.contour = Contour::ALL[i.min(Contour::ALL.len() - 1)];
                }
                if let Some(v) = self.tool_knob(c, input, 1, cell(1), &RANGE, f32::from(st.range)) {
                    s.range = v.round() as u8;
                }
                if let Some(v) = self.tool_knob(c, input, 2, cell(2), &DENSITY, st.density) {
                    s.density = v;
                }
                let oct = ParamInfo { min: 0.0, max: 7.0, ..OCTAVE };
                if let Some(i) = self.tool_choice(c, input, 3, cell(3), &oct, st.mel_octave as usize) {
                    s.mel_octave = i.clamp(1, 7) as i32;
                }
            }
            Tool::Bassline => {
                if let Some(i) = self.tool_choice(c, input, 0, cell(0), &BASS_RHYTHM, pick(&BassRhythm::ALL, st.bass_rhythm)) {
                    s.bass_rhythm = BassRhythm::ALL[i.min(BassRhythm::ALL.len() - 1)];
                }
                let oct = ParamInfo { min: 0.0, max: 4.0, ..OCTAVE };
                if let Some(i) = self.tool_choice(c, input, 1, cell(1), &oct, st.bass_octave as usize) {
                    s.bass_octave = i.clamp(0, 4) as i32;
                }
                // What it follows: a channel's chords, or the mood's progression.
                let ch = self.piano_channel().unwrap_or(0);
                let mut sources: Vec<Option<usize>> = vec![None];
                sources.extend(self.channels_with_notes(ch).into_iter().map(Some));
                let cur = sources.iter().position(|&x| x == st.bass_source).unwrap_or(0);
                let name = |app: &App, s: Option<usize>| s.map_or_else(|| tr("Mood progression").to_string(), |c| app.project.channels[c].name.clone());
                let follow = Rect::new(cell(2).x, area.y, CELL * 2.0 + 8.0, area.h);
                if let Some(d) = self.cycle_field(c, input, wid("tool-follow", 0, 0), follow, tr("Follows"), &name(self, sources[cur])) {
                    s.bass_source = sources[(cur as i32 + d).rem_euclid(sources.len() as i32) as usize];
                }
                if s.bass_source.is_none() {
                    let r = Rect::new(area.x, area.y + 62.0, CELL, area.h);
                    if let Some(i) = self.tool_choice(c, input, 4, r, &MOOD, pick(&Mood::ALL, st.mood)) {
                        s.mood = Mood::ALL[i.min(Mood::ALL.len() - 1)];
                    }
                    let r = Rect::new(area.x + CELL + 8.0, area.y + 62.0, CELL, area.h);
                    if let Some(i) = self.tool_choice(c, input, 5, r, &CHORD_LEN, st.chord_len) {
                        s.chord_len = i.min(CHORD_LENS.len() - 1);
                    }
                }
            }
            Tool::Chords => {
                if let Some(i) = self.tool_choice(c, input, 0, cell(0), &MOOD, pick(&Mood::ALL, st.mood)) {
                    s.mood = Mood::ALL[i.min(Mood::ALL.len() - 1)];
                }
                if let Some(i) = self.tool_choice(c, input, 1, cell(1), &CHORD_LEN, st.chord_len) {
                    s.chord_len = i.min(CHORD_LENS.len() - 1);
                }
                let oct = ParamInfo { min: 0.0, max: 6.0, ..OCTAVE };
                if let Some(i) = self.tool_choice(c, input, 2, cell(2), &oct, st.chord_octave as usize) {
                    s.chord_octave = i.clamp(2, 6) as i32;
                }
            }
        }
        self.piano.tool = s;
    }

    /// Rows of settings a tool shows.
    fn tool_rows(&self, tool: Tool) -> usize {
        match tool {
            Tool::Bassline if self.piano.tool.bass_source.is_none() => 2,
            _ => 1,
        }
    }

    /// Whether a re-roll changes the result.
    fn tool_seeded(&self, tool: Tool) -> bool {
        match tool {
            Tool::Arpeggiate => self.piano.tool.arp_dir == ArpDir::Random,
            Tool::Humanize | Tool::Rhythm | Tool::Melody | Tool::Bassline | Tool::Chords => true,
            _ => false,
        }
    }

    /// A small selectable chip, like the cells of the chord popup.
    fn choice_chip(&mut self, c: &mut Canvas, r: Rect, id: u64, hov: bool, on: bool, label: &str) {
        let t = self.theme;
        let hv = self.hover_anim(id, hov);
        let k = self.anim.to(id ^ 0x6f6e, on as u8 as f32, T_HOVER);
        c.fill_rrect(r, 8.0, t.raised.mix(t.raised_hi, hv).mix(t.accent, k));
        let (cx, cy) = r.center();
        self.text_center(c, self.f.sans_med, 12.0, cx, cy, label, t.text2.mix(t.text, hv).mix(t.on_accent, k));
        if hov {
            self.set_cursor(ponzik_window::Cursor::Hand);
        }
    }

    /// A field showing `text` that cycles with clicks (left part: back) and
    /// the wheel; returns the step taken.
    fn cycle_field(&mut self, c: &mut Canvas, input: &Input, id: u64, cell: Rect, label: &str, text: &str) -> Option<i32> {
        let t = self.theme;
        self.text_center(c, self.f.sans, 12.0, cell.x + cell.w * 0.5, cell.y + 6.0, label, t.text2);
        let field = Rect::new(cell.x + 2.0, cell.y + 20.0, cell.w - 4.0, 30.0);
        let hov = self.hovered(input, id, field);
        let hv = self.hover_anim(id, hov);
        c.fill_rrect(field, 8.0, t.raised.mix(t.raised_hi, hv));
        c.stroke_rrect(field, 8.0, 1.0, t.hairline);
        let (fx, fy) = field.center();
        draw_icon(c, Icon::ChevronLeft, field.x + 10.0, fy, 12.0, t.muted.mix(t.text2, hv));
        draw_icon(c, Icon::ChevronRight, field.right() - 10.0, fy, 12.0, t.muted.mix(t.text2, hv));
        let shown = self.fit(self.f.sans_med, 12.0, text, field.w - 40.0);
        self.text_center(c, self.f.sans_med, 12.0, fx, fy, &shown, t.text);
        if hov {
            self.set_cursor(ponzik_window::Cursor::Hand);
        }
        if hov && input.pressed {
            return Some(if input.x < field.x + field.w * 0.3 { -1 } else { 1 });
        }
        (hov && input.wheel != 0.0).then(|| -(input.wheel.signum() as i32))
    }

    /// Before and after, as a small piano roll: the notes that go as outlines,
    /// the result filled.
    fn tool_preview(&mut self, c: &mut Canvas, r: Rect, res: &ToolResult, generator: bool, reveal: f32) {
        let t = self.theme;
        c.fill_rrect(r, 10.0, t.well);
        // A generator frames its result; what it replaces may run off the edges.
        let framed: &[Note] = if generator && !res.after.is_empty() { &[] } else { &res.before };
        let both = framed.iter().chain(&res.after);
        let (mut lo_t, mut hi_t, mut lo_k, mut hi_k) = (u32::MAX, 0u32, 127u8, 0u8);
        for n in both {
            lo_t = lo_t.min(n.start);
            hi_t = hi_t.max(n.start + n.len.max(1));
            lo_k = lo_k.min(n.key);
            hi_k = hi_k.max(n.key);
        }
        if generator || lo_t == u32::MAX {
            lo_t = 0;
            hi_t = hi_t.max(self.project.patterns[self.project.current_pattern].len.max(TICKS_PER_BAR));
        } else {
            lo_t = lo_t / 96 * 96;
            hi_t = hi_t.div_ceil(96) * 96;
        }
        if lo_k > hi_k {
            (lo_k, hi_k) = (54, 66);
        }
        let mid = (u16::from(lo_k) + u16::from(hi_k)) as f32 * 0.5;
        let span = (f32::from(hi_k - lo_k) + 3.0).max(12.0);
        let (k0, k1) = (mid - span * 0.5, mid + span * 0.5);
        let inner = r.inset(8.0);
        let x_of = |tick: f32| inner.x + (tick - lo_t as f32) / (hi_t - lo_t).max(1) as f32 * inner.w;
        let row = inner.h / (k1 - k0);
        let y_of = |key: f32| inner.y + (k1 - key - 1.0) * row + row * 0.5;
        let prev = c.push_clip(r);
        // Rows of the scale, then beats and bars.
        if self.project.scale != ScaleKind::Chromatic {
            for k in (k0.floor() as i32)..=(k1.ceil() as i32) {
                if (0..128).contains(&k) && !self.project.in_scale(k as u8) {
                    c.fill_rect(Rect::new(inner.x, y_of(k as f32), inner.w, row), t.bg_deep.alpha(0.35));
                }
            }
        }
        let mut tick = lo_t;
        while tick <= hi_t {
            let x = x_of(tick as f32).round();
            let a = if tick % TICKS_PER_BAR == 0 { 2.6 } else { 1.0 };
            c.fill_rect(Rect::new(x, inner.y, 1.0, inner.h), t.hairline.alpha(a));
            tick += 96;
        }
        let h = (row - 1.0).clamp(4.0, 9.0);
        let rect = |n: &Note| Rect::new(x_of(n.start as f32), y_of(n.key as f32) - h * 0.5, (x_of((n.start + n.len.max(1)) as f32) - x_of(n.start as f32) - 1.0).max(2.0), h);
        let ghost = if generator { t.negative.alpha(0.45) } else { t.text.alpha(0.28) };
        for n in &res.before {
            c.stroke_rrect(rect(n), 2.0, 1.0, ghost);
        }
        for (i, n) in res.after.iter().enumerate() {
            // The result fills in from left to right when the panel opens.
            let k = ((reveal * 1.6 - i as f32 / res.after.len().max(1) as f32 * 0.6).clamp(0.0, 1.0)).powf(0.7);
            if k <= 0.0 {
                continue;
            }
            let b = rect(n);
            let fill = t.accent_deep.mix(t.accent, 0.35 + 0.65 * n.vel).alpha(k * (0.35 + 0.65 * f32::from(n.prob) / 100.0));
            c.fill_rrect(b, 2.0, fill);
            if n.ratchet > 1 && b.w > 6.0 {
                for j in 1..n.ratchet {
                    let x = b.x + b.w * f32::from(j) / f32::from(n.ratchet);
                    c.fill_rect(Rect::new(x.round(), b.y, 1.0, b.h), t.bg_deep.alpha(0.8 * k));
                }
            }
        }
        c.set_clip(Some(prev));
    }

    /// The Euclidean pattern as a ring of steps, the pulses lit and joined.
    fn euclid_ring(&mut self, c: &mut Canvas, r: Rect, reveal: f32) {
        let t = self.theme;
        c.fill_rrect(r, 10.0, t.well);
        let (steps, pulses, rot) = self.piano.tool.euclid;
        let hits = generate::euclid(steps.max(1), pulses, rot);
        let (cx, cy) = r.center();
        let rad = r.w * 0.5 - 16.0;
        let at = |i: usize| {
            let a = std::f32::consts::TAU * i as f32 / hits.len() as f32 - std::f32::consts::FRAC_PI_2;
            (cx + a.cos() * rad, cy + a.sin() * rad)
        };
        c.stroke_circle(cx, cy, rad, 1.0, t.hairline.alpha(2.0));
        let lit: Vec<(f32, f32)> = (0..hits.len()).filter(|&i| hits[i]).map(at).collect();
        if lit.len() >= 2 {
            let mut poly = lit.clone();
            poly.push(lit[0]);
            c.stroke_polyline(&poly, 1.5, t.accent.alpha(0.45 * reveal));
        }
        for (i, &on) in hits.iter().enumerate() {
            let (x, y) = at(i);
            if on {
                c.fill_circle(x, y, 5.5, t.accent.alpha(0.4 + 0.6 * reveal));
            } else {
                c.fill_circle(x, y, 3.0, t.muted.alpha(0.5));
            }
        }
        self.text_center(c, self.f.mono_med, 12.0, cx, cy, &format!("E({pulses},{steps})"), t.text2);
    }

    /// A tool's panel: settings, live preview, re-roll and apply. Returns
    /// false when it closes.
    pub(super) fn tool_popup(&mut self, c: &mut Canvas, input: &Input, tool: Tool, anchor: Rect, screen: Rect) -> bool {
        let t = self.theme;
        let open = self.anim.to(wid("popup", 0, 0), 1.0, T_HOVER);
        let rows = self.tool_rows(tool);
        let (w, preview_h) = (440.0, 150.0);
        let h = 74.0 + preview_h + 14.0 + rows as f32 * 70.0 + 58.0;
        let x = anchor.x.min(screen.right() - 12.0 - w).max(12.0);
        let mut y0 = anchor.bottom() + 8.0;
        if y0 + h > screen.bottom() - 8.0 {
            y0 = (screen.bottom() - 8.0 - h).max(8.0);
        }
        let r = Rect::new(x, y0 + (1.0 - open) * 6.0, w, h);
        c.fill_rrect(r.translate(0.0, 6.0).inset(-2.0), 16.0, Color::rgba(0, 0, 0, (90.0 * open) as u8));
        c.fill_rrect(r, 14.0, t.panel);
        c.stroke_rrect(r, 14.0, 1.0, t.hairline.alpha(1.6));

        // Title, what it does, on what.
        let badge = Rect::new(r.x + 14.0, r.y + 14.0, 34.0, 34.0);
        c.fill_rrect(badge, 10.0, t.accent.alpha(0.16));
        draw_icon(c, tool.icon(), badge.x + 17.0, badge.y + 17.0, 18.0, t.accent);
        self.text(c, self.f.sans_med, 15.0, badge.right() + 10.0, r.y + 24.0, tr(tool.name()), t.text);
        self.text_fit(c, self.f.sans, 12.0, badge.right() + 10.0, r.y + 42.0, tr(tool.tip()), w - 80.0 - 120.0, t.muted);
        let res = self.tool_result(tool);
        let target = match (&res, tool.generator()) {
            (None, _) => tr("No notes").to_string(),
            (Some(res), true) if !res.before.is_empty() => {
                tr_fmt_n!("Replaces {} note", "Replaces {} notes", res.before.len(), res.before.len())
            }
            (Some(res), true) => self.project.channels[res.ch].name.clone(),
            (Some(res), false) => tr_fmt!("{} → {}", tr_fmt_n!("{} note", "{} notes", res.before.len(), res.before.len()), res.after.len()),
        };
        self.text_right(c, self.f.mono_med, 12.0, r.right() - 16.0, r.y + 24.0, &target, t.text2);

        let preview = Rect::new(r.x + 14.0, r.y + 62.0, w - 28.0, preview_h);
        match &res {
            Some(res) => {
                let reveal = self.anim.to(wid("tool-reveal", 0, 0), 1.0, 0.25);
                let mut roll = preview;
                if tool == Tool::Euclid {
                    let ring = Rect::new(preview.x, preview.y, preview.h, preview.h);
                    self.euclid_ring(c, ring, reveal);
                    roll = Rect::new(ring.right() + 8.0, preview.y, preview.w - ring.w - 8.0, preview.h);
                }
                self.tool_preview(c, roll, res, tool.generator(), reveal);
            }
            None => {
                c.fill_rrect(preview, 10.0, t.well);
                let (cx, cy) = preview.center();
                self.text_center(c, self.f.sans, 12.0, cx, cy, tr("Write or select notes first"), t.muted);
            }
        }

        let area = Rect::new(r.x + 14.0, preview.bottom() + 14.0, w - 28.0, 66.0);
        let before = self.piano.tool.clone();
        self.tool_controls(c, input, tool, area);

        // Footer: re-roll, apply.
        let foot_y = r.bottom() - 46.0;
        let mut rerolled = false;
        if self.tool_seeded(tool) {
            let b = Rect::new(r.x + 14.0, foot_y, 120.0, 34.0);
            if self.lab_button(c, input, wid("tool-reroll", 0, 0), b, Some(Icon::Reroll), tr("Re-roll"), 0, tr("Another take, same settings")) {
                rerolled = true;
            }
            self.text(c, self.f.mono, 11.0, b.right() + 10.0, foot_y + 17.0, &format!("#{}", self.piano.tool.seed % 10_000), t.muted);
        }
        let apply_r = Rect::new(r.right() - 14.0 - 120.0, foot_y, 120.0, 34.0);
        let can = res.is_some();
        let label = if tool.generator() { tr("Generate") } else { tr("Apply") };
        let apply = can && self.lab_button(c, input, wid("tool-apply", 0, 0), apply_r, Some(Icon::Check), label, 1, tr("Enter"));
        let enter = can && input.keys.iter().any(|(k, _)| *k == Key::Enter);
        if rerolled {
            self.piano.tool.seed = ponzik_engine::splitmix(self.piano.tool.seed ^ (self.time.to_bits() as u64)) % 1_000_000 + 1;
        }
        if rerolled || self.piano.tool != before {
            self.anim.set(wid("tool-reveal", 0, 0), 0.35);
        }
        if apply || enter {
            self.apply_tool(tool);
            return false;
        }
        let clicked = input.pressed || input.right_pressed;
        !clicked || r.contains(input.x, input.y) || anchor.contains(input.x, input.y)
    }
}

const CHANCE: ParamInfo = ParamInfo::new(0, tr_noop("Chance"), 0.0, 1.0, 1.0, Unit::Percent);
const CONDITION: ParamInfo = ParamInfo::choice(0, tr_noop("Condition"), &COND_LABELS, 0);
const REPEAT_LABELS: [&str; 8] = ["1", "2", "3", "4", "5", "6", "7", "8"];
const REPEATS: ParamInfo = ParamInfo::choice(0, tr_noop("Repeats"), &REPEAT_LABELS, 0);
const STEP_VEL: ParamInfo = ParamInfo::new(0, tr_noop("Velocity"), 0.05, 1.0, 0.8, Unit::Percent);

impl App {
    /// Draws which of the next `passes` passes play under `cond`, as dots.
    pub(super) fn pass_dots(&mut self, c: &mut Canvas, x: f32, cy: f32, cond: NoteCond, passes: i64, prob: u8) {
        let t = self.theme;
        for p in 0..passes {
            let cx = x + p as f32 * 14.0;
            if cond.holds(p) {
                c.fill_circle(cx, cy, 4.5, t.accent.alpha(0.35 + 0.65 * f32::from(prob) / 100.0));
            } else {
                c.stroke_circle(cx, cy, 4.0, 1.2, t.muted.alpha(0.6));
            }
        }
    }

    /// Chance, condition, repeats and velocity of a step of the Channel Rack.
    /// Returns false when it closes.
    pub(super) fn step_popup(&mut self, c: &mut Canvas, input: &Input, ch: usize, step: usize, anchor: Rect, screen: Rect) -> bool {
        let t = self.theme;
        let Some(note) = self.note_in_step(ch, step) else { return false };
        let open = self.anim.to(wid("popup", 0, 0), 1.0, T_HOVER);
        let (w, h) = (4.0 * 84.0 + 3.0 * 8.0 + 28.0, 222.0);
        let x = (anchor.x + anchor.w * 0.5 - w * 0.5).clamp(12.0, screen.right() - 12.0 - w);
        let mut y0 = anchor.bottom() + 8.0;
        if y0 + h > screen.bottom() - 8.0 {
            y0 = (anchor.y - 8.0 - h).max(8.0);
        }
        let r = Rect::new(x, y0 + (1.0 - open) * 6.0, w, h);
        c.fill_rrect(r.translate(0.0, 6.0).inset(-2.0), 16.0, Color::rgba(0, 0, 0, (90.0 * open) as u8));
        c.fill_rrect(r, 14.0, t.panel);
        c.stroke_rrect(r, 14.0, 1.0, t.hairline.alpha(1.6));
        let name = self.project.channels.get(ch).map_or(String::new(), |c| c.name.clone());
        self.text(c, self.f.sans_med, 14.0, r.x + 16.0, r.y + 22.0, &tr_fmt!("Step {}", step + 1), t.text);
        self.text_right(c, self.f.sans, 12.0, r.right() - 16.0, r.y + 22.0, &name, t.muted);

        // The step: its repeats over its length, faded by its chance.
        let strip = Rect::new(r.x + 16.0, r.y + 40.0, w - 32.0, 34.0);
        c.fill_rrect(strip, 8.0, t.well);
        let reps = note.ratchet.max(1);
        let seg = (strip.w - 8.0) / f32::from(reps);
        for k in 0..reps {
            let b = Rect::new(strip.x + 4.0 + f32::from(k) * seg, strip.y + 5.0, seg - 3.0, strip.h - 10.0);
            let fill = t.accent_deep.mix(t.accent, 0.4 + 0.6 * note.vel).alpha(0.3 + 0.7 * f32::from(note.prob) / 100.0);
            c.fill_rrect(b, 4.0, fill);
        }
        // Which passes play.
        self.text(c, self.f.sans, 11.0, r.x + 16.0, r.y + 90.0, tr("Passes"), t.muted);
        self.pass_dots(c, r.x + 74.0, r.y + 90.0, note.cond, 8, note.prob);

        let cell = |i: usize| Rect::new(r.x + 14.0 + i as f32 * 92.0, r.y + 108.0, 84.0, 70.0);
        let mut n = note;
        if let Some(v) = self.knob(c, input, wid("step-ctl", 0, 0), cell(0), &CHANCE, f32::from(note.prob) / 100.0) {
            n.prob = (v * 100.0).round().clamp(0.0, 100.0) as u8;
        }
        if let Some(v) = self.choice(c, input, wid("step-ctl", 1, 0), cell(1), &CONDITION, cond_index(note.cond) as f32) {
            n.cond = CONDS[(v.round().max(0.0) as usize).min(CONDS.len() - 1)];
        }
        if let Some(v) = self.choice(c, input, wid("step-ctl", 2, 0), cell(2), &REPEATS, f32::from(note.ratchet.max(1) - 1)) {
            n.ratchet = (v.round().clamp(0.0, 7.0) as u8) + 1;
        }
        if let Some(v) = self.knob(c, input, wid("step-ctl", 3, 0), cell(3), &STEP_VEL, note.vel) {
            n.vel = v;
        }
        let hint = tr("Alt+click a step · Esc to close");
        self.text_center(c, self.f.sans, 11.0, r.x + w * 0.5, r.bottom() - 22.0, hint, t.muted);
        if n != note {
            let pat = self.project.current_pattern;
            let (lo, hi) = (step as u32 * TICKS_PER_STEP, (step as u32 + 1) * TICKS_PER_STEP);
            if let Some(notes) = self.project.patterns[pat].notes.get_mut(ch) {
                for m in notes.iter_mut().filter(|m| m.start >= lo && m.start < hi) {
                    (m.prob, m.cond, m.ratchet, m.vel) = (n.prob, n.cond, n.ratchet, n.vel);
                }
            }
            self.commit(tr("Step settings"));
        }
        let clicked = input.pressed || input.right_pressed;
        !clicked || r.contains(input.x, input.y) || anchor.contains(input.x, input.y)
    }
}

impl App {
    /// Screenshots: chances, conditions and repeats on some notes of the
    /// current pattern; `spec` is the piano roll lane shown (0 to 2).
    pub fn screenshot_vary(&mut self, spec: &str) {
        let pat = self.project.current_pattern;
        for notes in &mut self.project.patterns[pat].notes {
            for (i, n) in notes.iter_mut().enumerate() {
                match i % 6 {
                    1 => n.prob = 45,
                    2 => n.ratchet = 3,
                    3 => n.cond = NoteCond::Every(1, 2),
                    4 => (n.prob, n.ratchet) = (70, 2),
                    5 => n.cond = NoteCond::NotFirst,
                    _ => {}
                }
            }
        }
        self.commit(tr("Note chance"));
        self.piano.lane = super::lanes::PianoLane::note(spec.trim().parse::<usize>().unwrap_or(0).min(2));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::app::Focus;

    fn app() -> App {
        let project = ponzik_project::demo_project();
        let (_engine, handle) = project.build_engine(48_000.0, &|_| None);
        App::new(handle, project, 48_000)
    }

    #[test]
    fn every_tool_is_one_undo_step() {
        for tool in Tool::TRANSFORMS.into_iter().chain(Tool::GENERATORS) {
            let mut app = app();
            app.focus = Focus::Channel(4);
            app.project.key = 9;
            app.project.scale = ScaleKind::Minor;
            app.piano.tool.arp_dir = ArpDir::Random;
            app.commit("Scale");
            app.break_undo_merge();
            let before = app.project.clone();
            let steps = app.history_len();
            app.piano.selected = vec![0, 1, 2];
            app.time += 5.0;
            app.apply_tool(tool);
            if app.project == before {
                // Some tools leave these notes alone (no chords to strum, nothing to glue).
                continue;
            }
            assert_eq!(app.history_len(), steps + 1, "{tool:?}");
            app.undo();
            assert!(app.project == before, "{tool:?}");
        }
    }

    #[test]
    fn transforms_keep_the_rest_and_generators_replace() {
        let mut app = app();
        app.focus = Focus::Channel(4);
        let all = app.notes(4).to_vec();
        app.piano.selected = vec![1, 2];
        app.apply_tool(Tool::Reverse);
        let after = app.notes(4).to_vec();
        assert_eq!(after.len(), all.len());
        assert!(after.contains(&all[0]) && after.contains(&all[3]), "the unselected notes stay");
        assert_eq!(app.piano.selected.len(), 2, "the result stays selected");

        app.project.key = 9;
        app.project.scale = ScaleKind::Minor;
        app.apply_tool(Tool::Melody);
        let melody = app.notes(4).to_vec();
        assert!(!melody.is_empty());
        assert!(melody.iter().all(|n| ScaleKind::Minor.contains(9, n.key)));
        assert!(app.piano.selected.is_empty());
        // Same seed, same melody; a re-roll changes it.
        app.apply_tool(Tool::Melody);
        assert_eq!(app.notes(4), &melody[..]);
        app.piano.tool.seed += 1;
        app.apply_tool(Tool::Melody);
        assert_ne!(app.notes(4), &melody[..]);
    }

    #[test]
    fn the_bassline_follows_a_chord_channel() {
        let mut app = app();
        let bar = TICKS_PER_BAR;
        app.project.patterns[0].len = 2 * bar;
        // Channel 3 holds C major then A minor; the bassline goes on channel 4.
        app.project.patterns[0].notes[3] =
            [(0, 48), (0, 52), (0, 55), (bar, 45), (bar, 48), (bar, 52)].map(|(s, k)| Note::new(s, bar, k, 0.8)).to_vec();
        app.focus = Focus::Channel(4);
        app.open_tool(Tool::Bassline, Rect::new(0.0, 0.0, 1.0, 1.0));
        assert_eq!(app.piano.tool.bass_source, Some(3));
        app.piano.tool.bass_rhythm = BassRhythm::Sustained;
        app.apply_tool(Tool::Bassline);
        assert_eq!(app.notes(4).iter().map(|n| (n.start, n.key)).collect::<Vec<_>>(), [(0, 36), (bar, 45)]);
    }
}
