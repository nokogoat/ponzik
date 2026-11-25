//! Key detection, the chord track of the piano roll and grooves taken from
//! audio clips (the pool itself is in `groovepool.rs`).

use ponzik_engine::{AutoScope, AutoTarget, Note};
use ponzik_i18n::{tr, tr_fmt};
use ponzik_project::groove::{Groove, groove_from_audio};
use ponzik_project::harmony::{KeyGuess, detect_key};
use ponzik_project::ScaleKind;
use ponzik_ui::{Canvas, Rect};

use super::Input;
use super::app::{Action, App, MenuItem, Popup, key_name};
use super::lanes::PianoLane;
use super::widgets::Icon;

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
                let id = self.project.add_groove(g);
                self.groove_ui.sel = Some(id);
                self.commit(tr("Extract a groove"));
                self.toast(&tr_fmt!("Groove \"{}\" saved in the pool", name));
            }
            None => self.toast(tr("No attacks found in the clip")),
        }
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
                    let id = self.project.add_groove(Groove { amount: 0.8, ..Groove::swing(66.0) });
                    self.project.set_pattern_groove(pat, Some(id));
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
