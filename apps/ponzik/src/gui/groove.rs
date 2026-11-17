//! Groove pool, key detection and the chord track of the piano roll.

use ponzik_engine::{AutoScope, AutoTarget, Note, ParamInfo, TICKS_PER_STEP, Unit};
use ponzik_i18n::{tr, tr_fmt, tr_noop};
use ponzik_project::groove::{GROOVE_STEPS, Groove, PatternGroove, groove_from_audio};
use ponzik_project::harmony::{KeyGuess, detect_key};
use ponzik_project::ScaleKind;
use ponzik_ui::{Canvas, Color, Rect};

use super::Input;
use super::app::{Action, App, MenuItem, Popup, key_name};
use super::lanes::PianoLane;
use super::theme::*;
use super::widgets::{Icon, wid};

/// Where a key is detected from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeySource {
    /// The current pattern's notes.
    Pattern,
    /// Every pattern, weighted by how often the playlist plays it.
    Project,
    /// An audio clip of the playlist, by index.
    Audio(usize),
}

static TIMING: ParamInfo = ParamInfo::new(0, tr_noop("Timing"), 0.0, 1.0, 1.0, Unit::Percent);
static VELOCITY: ParamInfo = ParamInfo::new(1, tr_noop("Velocity"), 0.0, 1.0, 1.0, Unit::Percent);

/// "A minor", "C major": a detected key, in the interface's language.
pub fn key_label(key: u8, scale: ScaleKind) -> String {
    format!("{} {}", key_name(key), tr(scale.name()).to_lowercase())
}

impl App {
    /// Channels whose instrument plays pitches.
    fn melodic_channels(&mut self) -> Vec<bool> {
        let kinds: Vec<String> = self.project.channels.iter().map(|c| c.instrument.kind.clone()).collect();
        kinds.iter().map(|k| self.is_melodic(k)).collect()
    }

    /// Detects the key from `source` and offers it in a menu.
    pub(super) fn detect_key(&mut self, source: KeySource, anchor: Rect) {
        let profile = match source {
            KeySource::Pattern | KeySource::Project => {
                let melodic = self.melodic_channels();
                let only = (source == KeySource::Pattern).then_some(self.project.current_pattern);
                self.project.key_profile(only, |ch| melodic.get(ch).copied().unwrap_or(false))
            }
            KeySource::Audio(i) => {
                let Some(clip) = self.project.audio_clips.get(i) else { return };
                let Some(sample) = self.samples.get(&clip.path).cloned() else {
                    self.toast(tr("The clip's file is not loaded"));
                    return;
                };
                let map = self.project.tempo_map();
                let secs = map.secs_at((clip.start + clip.len) as f64) - map.secs_at(clip.start as f64);
                ponzik_project::harmony::audio_profile(&sample, clip.offset, Some(secs))
            }
        };
        let guesses: Vec<KeyGuess> = detect_key(&profile).into_iter().take(5).collect();
        let Some(best) = guesses.first().copied() else {
            self.toast(match source {
                KeySource::Audio(_) => tr("No pitched sound to detect a key from"),
                _ => tr("No melodic notes to detect a key from"),
            });
            return;
        };
        if matches!(source, KeySource::Audio(_)) {
            let text = tr_fmt!("Detected: {}", key_label(best.key, best.scale));
            self.toast_action(&text, tr("Apply"), Action::SetKey(best.key, best.scale));
        }
        self.open_popup(Popup::KeyDetect { source, guesses }, anchor);
    }

    /// Menu of the detected keys: the best one first, to apply.
    pub(super) fn key_menu(&self, source: KeySource, guesses: &[KeyGuess]) -> (String, Vec<MenuItem>) {
        let Some(best) = guesses.first() else { return (String::new(), Vec::new()) };
        let pct = |g: &KeyGuess| format!("{:.0}%", (g.score.max(0.0) * 100.0).round());
        let current = |g: &KeyGuess| self.project.key == g.key && self.project.scale == g.scale;
        let title = tr_fmt!("Detected: {} · {}", key_label(best.key, best.scale), pct(best));
        let mut v = vec![
            MenuItem::new(Icon::Check, &tr_fmt!("Apply {}", key_label(best.key, best.scale)), Action::SetKey(best.key, best.scale))
                .detail(pct(best))
                .on(current(best))
                .tip(tr("Sets the project's key and scale: the piano roll shows it, snaps and chords follow it")),
        ];
        for (i, g) in guesses.iter().enumerate().skip(1) {
            let it = MenuItem::new(Icon::Scale, &key_label(g.key, g.scale), Action::SetKey(g.key, g.scale)).detail(pct(g)).on(current(g));
            v.push(if i == 1 { it.sep() } else { it });
        }
        match source {
            KeySource::Pattern => v.push(MenuItem::new(Icon::Detect, tr("Detect in the whole project"), Action::DetectKey(KeySource::Project)).sep()),
            KeySource::Project => v.push(MenuItem::new(Icon::Detect, tr("Detect in this pattern"), Action::DetectKey(KeySource::Pattern)).sep()),
            KeySource::Audio(_) => {}
        }
        (title, v)
    }

    /// Saves the groove of an audio clip's attacks in the pool.
    pub(super) fn extract_audio_groove(&mut self, i: usize) {
        let Some(clip) = self.project.audio_clips.get(i).cloned() else { return };
        let Some(sample) = self.samples.get(&clip.path).cloned() else {
            self.toast(tr("The clip's file is not loaded"));
            return;
        };
        let name = std::path::Path::new(&clip.path).file_stem().map_or_else(|| tr("Audio").to_string(), |s| s.to_string_lossy().into_owned());
        match groove_from_audio(&clip, &sample, &self.project.tempo_map(), name.clone()) {
            Some(g) => {
                self.project.grooves.push(g);
                self.commit(tr("Extract a groove"));
                self.toast(&tr_fmt!("Groove \"{}\" saved in the pool", name));
            }
            None => self.toast(tr("No attacks found in the clip")),
        }
    }

    /// Sets the groove of the current pattern (`None`: straight).
    pub(super) fn set_groove(&mut self, groove: Option<Groove>) {
        let pat = self.project.current_pattern;
        let p = &mut self.project.patterns[pat];
        p.groove = groove.map(|g| {
            let (timing, velocity) = p.groove.as_ref().map_or((1.0, 1.0), |old| (old.timing, old.velocity));
            PatternGroove { groove: g, timing, velocity }
        });
        self.commit(tr("Groove"));
    }

    /// Groove pool: the pattern's groove, its amounts, extract and commit.
    /// False when it closes.
    pub(super) fn groove_popup(&mut self, c: &mut Canvas, input: &Input, anchor: Rect, screen: Rect) -> bool {
        let t = self.theme;
        let open = self.anim.to(wid("popup", 0, 0), 1.0, T_HOVER);
        const COLS: usize = 4;
        const GAP: f32 = 4.0;
        let (w, cell_h) = (392.0, 30.0);
        let cell_w = (w - 24.0 - (COLS - 1) as f32 * GAP) / COLS as f32;
        let pat = self.project.current_pattern;
        let pool = self.project.groove_pool();
        let current = self.project.patterns[pat].groove.clone();
        let n_cells = pool.len() + 1;
        let rows = n_cells.div_ceil(COLS) as f32;
        let viz_h = 64.0;
        let h = 40.0 + rows * (cell_h + GAP) + 12.0 + viz_h + 12.0 + 76.0 + 12.0 + 32.0 + 14.0;
        let x = anchor.x.min(screen.right() - 12.0 - w).max(12.0);
        let mut y0 = anchor.bottom() + 8.0;
        if y0 + h > screen.bottom() - 8.0 {
            y0 = (screen.bottom() - 8.0 - h).max(8.0);
        }
        let r = Rect::new(x, y0 + (1.0 - open) * 6.0, w, h);
        c.fill_rrect(r.translate(0.0, 6.0).inset(-2.0), 16.0, Color::rgba(0, 0, 0, (90.0 * open) as u8));
        c.fill_rrect(r, 14.0, t.panel);
        c.stroke_rrect(r, 14.0, 1.0, t.hairline.alpha(1.6));
        let title = tr_fmt!("Groove · {}", self.project.patterns[pat].name);
        let title = self.fit(self.f.sans_med, 12.0, &title, w - 32.0);
        self.text(c, self.f.sans_med, 12.0, r.x + 16.0, r.y + 20.0, &title, t.muted);
        let over = |b: Rect| input.inside && b.contains(input.x, input.y);

        // The pool: none, the swings, the project's grooves.
        let mut y = r.y + 34.0;
        let mut picked: Option<Option<Groove>> = None;
        for i in 0..n_cells {
            let cell = Rect::new(r.x + 12.0 + (i % COLS) as f32 * (cell_w + GAP), y + (i / COLS) as f32 * (cell_h + GAP), cell_w, cell_h);
            let (label, groove) = if i == 0 { (tr("None").to_string(), None) } else { (pool[i - 1].name.clone(), Some(pool[i - 1].clone())) };
            let cur = match (&current, &groove) {
                (None, None) => true,
                (Some(c), Some(g)) => c.groove == *g,
                _ => false,
            };
            let hov = over(cell);
            self.choice_cell(c, cell, wid("groove-cell", i, 0), hov, cur, &label);
            if hov {
                if let Some(g) = &groove {
                    self.set_tip(&tr_fmt!("{}\nUp to {:.0} ticks late · applied without moving the notes", g.name, g.depth()), "");
                }
                if input.pressed {
                    picked = Some(groove);
                }
            }
        }
        if let Some(g) = picked {
            self.set_groove(g);
        }
        y += rows * (cell_h + GAP) + 8.0;

        // The groove over a bar: where each sixteenth lands, how loud.
        let viz = Rect::new(r.x + 12.0, y, w - 24.0, viz_h);
        c.fill_rrect(viz, 8.0, t.well);
        let slot_w = viz.w / GROOVE_STEPS as f32;
        let px_per_tick = slot_w / TICKS_PER_STEP as f32;
        let pg = self.project.patterns[pat].groove.clone();
        for s in 0..GROOVE_STEPS {
            let gx = viz.x + (s as f32 + 0.5) * slot_w;
            let beat = s % 4 == 0;
            c.fill_rect(Rect::new(gx.round(), viz.y + 6.0, 1.0, viz.h - 12.0), t.hairline.alpha(if beat { 3.0 } else { 1.4 }));
            let (dt, vel) = pg.as_ref().map_or((0.0, 1.0), |g| {
                (g.groove.timing[s] * g.timing, 1.0 + (g.groove.velocity[s] - 1.0) * g.velocity)
            });
            let bar_h = ((viz.h - 16.0) * 0.6 * vel).clamp(3.0, viz.h - 12.0);
            let bx = gx + dt * px_per_tick;
            let col = if dt.abs() > 0.5 { t.accent_hot } else { t.accent };
            c.fill_rrect(Rect::new(bx - 3.0, viz.bottom() - 6.0 - bar_h, 6.0, bar_h), 2.0, col.alpha(0.9));
        }
        y += viz_h + 12.0;

        // Amounts.
        let (timing, velocity) = pg.as_ref().map_or((1.0, 1.0), |g| (g.timing, g.velocity));
        let k0 = Rect::new(r.x + 12.0, y, 84.0, 72.0);
        let k1 = Rect::new(k0.right() + 8.0, y, 84.0, 72.0);
        let t_new = self.knob(c, input, wid("groove-amt", 0, 0), k0, &TIMING, timing);
        let v_new = self.knob(c, input, wid("groove-amt", 1, 0), k1, &VELOCITY, velocity);
        if (t_new.is_some() || v_new.is_some())
            && let Some(g) = self.project.patterns[pat].groove.as_mut()
        {
            g.timing = t_new.unwrap_or(g.timing).clamp(0.0, 1.0);
            g.velocity = v_new.unwrap_or(g.velocity).clamp(0.0, 1.0);
            self.commit(tr("Groove amount"));
        }
        let note = Rect::new(k1.right() + 12.0, y + 8.0, r.right() - 12.0 - k1.right() - 12.0, 56.0);
        let lines = self.wrap(self.f.sans, 11.0, tr("The pattern plays the groove; its notes stay where they are until you commit."), note.w);
        for (i, l) in lines.iter().enumerate().take(4) {
            self.text(c, self.f.sans, 11.0, note.x, note.y + 8.0 + i as f32 * 14.0, l, t.muted);
        }
        y += 76.0 + 12.0;

        // Extract and commit.
        let bw = (w - 24.0 - 8.0) / 2.0;
        let extract = Rect::new(r.x + 12.0, y, bw, 32.0);
        if self.lab_button(c, input, wid("groove-extract", 0, 0), extract, Some(Icon::Groove), tr("Extract"), 0, tr("Saves this pattern's groove in the pool")) {
            match self.project.extract_groove(pat) {
                Some(_) => {
                    self.commit(tr("Extract a groove"));
                    self.toast(tr("Groove saved in the pool"));
                }
                None => self.toast(tr("No notes to take a groove from")),
            }
        }
        let commit = Rect::new(extract.right() + 8.0, y, bw, 32.0);
        let style = if pg.is_some() { 1 } else { 0 };
        if self.lab_button(c, input, wid("groove-commit", 0, 0), commit, Some(Icon::Check), tr("Commit"), style, tr("Moves the notes where the groove plays them")) {
            if self.project.commit_groove(pat) {
                self.piano.selected.clear();
                self.commit(tr("Commit the groove"));
            } else {
                self.toast(tr("Pick a groove first"));
            }
        }

        let clicked = input.pressed || input.right_pressed;
        !clicked || r.contains(input.x, input.y) || anchor.contains(input.x, input.y)
    }

    /// Chord names of the pattern above the piano roll, beat by beat (the
    /// selection's chords when several notes are selected). Read only.
    pub(super) fn chord_strip(&mut self, c: &mut Canvas, input: &Input, ch: usize, strip: Rect, x_of: &dyn Fn(f32) -> f32) {
        let t = self.theme;
        let pat = self.project.current_pattern;
        let melodic = self.melodic_channels();
        let selected: Vec<Note> = if self.piano.selected.len() >= 2 {
            self.piano.selected.iter().filter_map(|&i| self.notes(ch).get(i).copied()).collect()
        } else {
            Vec::new()
        };
        let only = (!selected.is_empty()).then_some(selected.as_slice());
        let spans = self.project.pattern_chords(pat, |c| melodic.get(c).copied().unwrap_or(false), only);
        let prev = c.push_clip(strip);
        for s in &spans {
            let (x0, x1) = (x_of(s.start as f32), x_of((s.start + s.len) as f32));
            if x1 < strip.x || x0 > strip.right() {
                continue;
            }
            let chip = Rect::new(x0 + 1.0, strip.y + 2.0, (x1 - x0 - 2.0).max(4.0), strip.h - 4.0);
            let fill = if only.is_some() { t.select.alpha(0.22) } else { t.accent.alpha(0.13) };
            c.fill_rrect(chip, 5.0, fill);
            let label = s.chord.label(key_name);
            let label = self.fit(self.f.mono_med, 11.0, &label, chip.w - 8.0);
            self.text(c, self.f.mono_med, 11.0, chip.x + 5.0, chip.y + chip.h * 0.5, &label, t.text.alpha(0.9));
        }
        c.set_clip(Some(prev));
        if input.inside && strip.contains(input.x, input.y) && self.drag.is_none() {
            self.set_tip(tr("Chords\nNamed beat by beat from the melodic channels · select notes to name only them"), "");
        }
    }

    /// Screenshot setup (`PONZIK_EXPRESS=slides|bend|cc|pan|fine|groove|chords|key`):
    /// slides and expressions on the bass line, a drawn pitch bend or mod
    /// wheel lane, a groove on the pattern.
    pub fn screenshot_express(&mut self, spec: &str) {
        let Some(ch) = self.piano_channel() else { return };
        let pat = self.project.current_pattern;
        for part in spec.split(',').map(str::trim) {
            match part {
                "slides" => {
                    let notes = &mut self.project.patterns[pat].notes[ch];
                    let picks: Vec<usize> = (0..notes.len()).filter(|i| i % 4 == 2).collect();
                    for i in picks {
                        let n = notes[i];
                        notes.push(Note { slide: true, start: n.start + n.len / 2, len: n.len.max(12), key: n.key.saturating_add(5), ..n });
                    }
                    notes.sort_by_key(|n| (n.start, n.key));
                    self.project.channels[ch].porta = 0.06;
                    self.piano.slide_mode = true;
                }
                "pan" | "fine" => {
                    for (i, n) in self.project.patterns[pat].notes[ch].iter_mut().enumerate() {
                        if part == "pan" {
                            n.pan = ((i as f32 * 0.7).sin() * 0.8 * 20.0).round() / 20.0;
                        } else {
                            n.fine = [0.0, 12.0, -25.0, 40.0, -8.0][i % 5];
                        }
                    }
                    self.piano.lane = if part == "pan" { PianoLane::Pan } else { PianoLane::Fine };
                }
                "bend" | "cc" => {
                    let target = if part == "bend" { AutoTarget::PitchBend(ch) } else { AutoTarget::Cc { channel: ch, cc: 1 } };
                    let scope = AutoScope::Pattern(pat);
                    let len = self.project.patterns[pat].len;
                    let mut tick = 0;
                    while tick <= len {
                        let x = tick as f32 / len as f32;
                        let v = if part == "bend" {
                            (x * std::f32::consts::TAU * 2.0).sin() * (1.0 - x) * 0.8
                        } else {
                            (0.15 + 0.8 * x * x).min(1.0)
                        };
                        self.project.add_automation_point(target, scope, tick, v);
                        tick += 12;
                    }
                    self.piano.lane = if part == "bend" { PianoLane::PitchBend } else { PianoLane::Cc(1) };
                }
                "groove" => {
                    let g = Groove::swing(66.0);
                    self.project.patterns[pat].groove = Some(PatternGroove { groove: g, timing: 0.8, velocity: 1.0 });
                }
                "chords" => {
                    // Am, F, C, G7, a beat each, over the bass.
                    let notes = &mut self.project.patterns[pat].notes[ch];
                    for (i, chord) in [[57u8, 60, 64, 0], [53, 57, 60, 0], [55, 60, 64, 0], [55, 59, 62, 65]].iter().enumerate() {
                        for &k in chord.iter().filter(|&&k| k > 0) {
                            notes.push(Note::new(i as u32 * 96, 96, k, 0.7));
                        }
                    }
                    notes.sort_by_key(|n| (n.start, n.key));
                }
                "key" => self.project.scale = ScaleKind::Chromatic,
                _ => {}
            }
        }
        self.commit(tr("Draw a controller"));
    }
}
