//! Clip panel (Live's Clip View): selecting a clip in the playlist shows it
//! at the bottom, in place of the devices. An audio clip gets a big waveform
//! with its fades (length and curve handles), warp markers and beat grid,
//! next to its gain, transposition, warp, reverse, loop and color; a pattern
//! clip gets its color, its name and « make unique ».

use std::sync::Arc;

use ponzik_engine::{PPQ, ParamInfo, Unit};
use ponzik_i18n::{tr, tr_fmt, tr_noop};
use ponzik_project::{AudioClipDef, WarpAlgo, WarpMarker, warp};
use ponzik_ui::{Canvas, Color, Path, Rect};
use ponzik_window::Cursor;

use super::Input;
use super::app::{App, Focus, Popup, Rename, View};
use super::playlist::PATTERN_COLORS;
use super::theme::*;
use super::widgets::{Icon, draw_icon, wid};

/// The clip shown in the clip panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipSel {
    Audio(usize),
    Pattern(usize),
}

const GAIN: ParamInfo = ParamInfo::new(0, tr_noop("Gain"), -36.0, 12.0, 0.0, Unit::Db);
const TRANSPOSE: ParamInfo =
    ParamInfo::new(1, tr_noop("Transpose"), -24.0, 24.0, 0.0, Unit::Semitones);
const FINE: ParamInfo = ParamInfo::new(2, tr_noop("Fine"), -50.0, 50.0, 0.0, Unit::Cents);
/// Shortest fade, in seconds (the engine's own floor).
const MIN_FADE: f32 = 0.002;
/// Height of the warp marker strip above the waveform.
const STRIP: f32 = 20.0;

fn db_to_gain(db: f32) -> f32 {
    if db <= GAIN.min + 0.01 {
        0.0
    } else {
        10f32.powf(db / 20.0)
    }
}

fn gain_to_db(g: f32) -> f32 {
    if g <= 1e-4 {
        GAIN.min
    } else {
        (20.0 * g.log10()).clamp(GAIN.min, GAIN.max)
    }
}

/// Short name of a mode.
fn mode_label(m: WarpAlgo) -> &'static str {
    match m {
        WarpAlgo::Beats => tr("Beats"),
        WarpAlgo::Tones => tr("Tones"),
        WarpAlgo::RePitch => tr("Re-Pitch"),
    }
}

/// Clip display color: its own, or the default audio clip color.
pub fn audio_color(c: &AudioClipDef, theme: &Theme) -> Color {
    if c.color == 0 {
        theme.select
    } else {
        Color::hex(c.color)
    }
}

impl App {
    /// Shows a clip in the clip panel (bottom panel, playlist view).
    pub fn select_clip(&mut self, sel: ClipSel) {
        self.clip_panel = Some((sel, self.focus));
        self.panel = true;
    }

    /// The clip panel's clip, while it is the one shown at the bottom.
    pub fn clip_panel_shown(&self) -> Option<ClipSel> {
        let (sel, focus) = self.clip_panel?;
        let valid = match sel {
            ClipSel::Audio(i) => i < self.project.audio_clips.len(),
            ClipSel::Pattern(i) => i < self.project.clips.len(),
        };
        (valid && self.view == View::Playlist && focus == self.focus).then_some(sel)
    }

    /// An audio clip was removed: the panel forgets it, or follows its neighbours.
    pub fn audio_clip_removed(&mut self, i: usize) {
        if let Some((ClipSel::Audio(j), focus)) = self.clip_panel {
            self.clip_panel = match j.cmp(&i) {
                std::cmp::Ordering::Equal => None,
                std::cmp::Ordering::Greater => Some((ClipSel::Audio(j - 1), focus)),
                std::cmp::Ordering::Less => self.clip_panel,
            };
        }
    }

    /// Point of the original file (seconds) heard at `tick` (absolute) in a
    /// clip: warp, loop and reverse included; `None` past the file.
    pub fn clip_file_secs(
        &self,
        c: &AudioClipDef,
        tick: f64,
        map: &ponzik_project::TempoMap,
        duration: f64,
    ) -> Option<f64> {
        let mut s = c.offset_at(tick, map);
        if c.looped && duration > 0.0 {
            s = s.rem_euclid(duration);
        }
        if !(0.0..duration).contains(&s) {
            return None;
        }
        Some(if c.reverse { duration - s } else { s })
    }

    /// Edits an audio clip. A clip that showed its whole file still does.
    pub fn edit_audio_clip(&mut self, i: usize, label: &str, f: impl FnOnce(&mut AudioClipDef)) {
        let Some(path) = self.project.audio_clips.get(i).map(|c| c.path.clone()) else {
            return;
        };
        let duration = self.samples.get(&path).map(|s| s.duration() as f64);
        let map = self.project.tempo_map();
        let c = &mut self.project.audio_clips[i];
        let whole = duration.filter(|&d| !c.looped && c.natural_len(d, &map).abs_diff(c.len) <= 1);
        f(c);
        if let Some(d) = whole
            && !c.looped
        {
            c.len = c.natural_len(d, &map);
        }
        self.commit(label);
    }

    /// Warp on or off; the first time, the original tempo is detected.
    pub fn toggle_warp(&mut self, i: usize) {
        let Some(c) = self.project.audio_clips.get(i) else {
            return;
        };
        let on = !c.warp;
        let bpm = if on && c.bpm <= 0.0 {
            Some(self.detect_clip_tempo(i))
        } else {
            None
        };
        let fallback = self.project.bpm;
        self.edit_audio_clip(i, tr("Warp clip"), |c| {
            c.warp = on;
            if let Some(b) = bpm {
                c.bpm = b.unwrap_or(fallback);
            }
        });
    }

    fn detect_clip_tempo(&mut self, i: usize) -> Option<f32> {
        let path = self.project.audio_clips.get(i)?.path.clone();
        let s = self.samples.get(&path)?.clone();
        warp::detect_tempo(&s)
    }

    /// Transients of a file (seconds), analysed once (first two minutes).
    fn file_onsets(&mut self, path: &str) -> Option<Arc<Vec<f64>>> {
        if let Some(o) = self.onsets.get(path) {
            return Some(o.clone());
        }
        let s = self.samples.get(path)?.clone();
        let sr = s.sample_rate.max(1.0);
        let n = s.len().min((120.0 * sr) as usize);
        let right = if s.right.len() == s.len() {
            &s.right
        } else {
            &s.left
        };
        let m = ponzik_dsp::onset::mono(&s.left[..n], &right[..n]);
        let a = ponzik_dsp::onset::OnsetAnalysis::new(&m, sr);
        let list: Vec<f64> = a
            .pick(&m, 0.6, 4096)
            .iter()
            .map(|o| o.pos as f64 / sr as f64)
            .collect();
        let list = Arc::new(list);
        self.onsets.insert(path.to_string(), list.clone());
        Some(list)
    }

    /// Bottom panel for the selected clip.
    pub(super) fn clip_panel(&mut self, c: &mut Canvas, input: &Input, r: Rect, sel: ClipSel) {
        let header = Rect::new(r.x + PAD, r.y + 10.0, r.w - 2.0 * PAD, 28.0);
        let body = Rect::new(
            r.x + PAD,
            header.bottom() + 10.0,
            r.w - 2.0 * PAD,
            r.bottom() - header.bottom() - 10.0 - 14.0,
        );
        let close = Rect::new(header.right() - 28.0, header.y, 28.0, 28.0);
        if self.icon_button(
            c,
            input,
            wid("cp-close", 0, 0),
            close,
            Icon::Close,
            false,
            (tr("Back to the devices"), ""),
        ) {
            self.clip_panel = None;
            return;
        }
        match sel {
            ClipSel::Audio(i) => self.audio_clip_panel(c, input, header, body, i),
            ClipSel::Pattern(i) => self.pattern_clip_panel(c, input, header, body, i),
        }
    }

    /// Title of the panel: color dot and name; returns where the title ends.
    fn clip_title(
        &mut self,
        c: &mut Canvas,
        input: &Input,
        header: Rect,
        color: Color,
        name: &str,
        rename: Option<Rename>,
    ) -> f32 {
        let t = self.theme;
        let cy = header.y + 14.0;
        c.fill_circle(header.x + 6.0, cy, 6.0, color);
        let tw = self
            .fonts
            .measure(self.f.sans_med, 15.0, name)
            .min(header.w * 0.4);
        let title = Rect::new(header.x + 14.0, header.y, tw + 12.0, 28.0);
        if let Some(rename) = rename {
            let id = wid("cp-title", 0, 0);
            let hov = self.hovered(input, id, title);
            let hv = self.hover_anim(id, hov);
            if hv > 0.01 {
                c.fill_rrect(title, 8.0, t.raised.alpha(hv));
            }
            if hov {
                self.set_tip(tr("Double-click: rename"), "");
                if input.double {
                    self.open_popup(
                        Popup::Rename {
                            target: rename,
                            text: name.to_string(),
                        },
                        title,
                    );
                }
            }
        }
        self.text_fit(
            c,
            self.f.sans_med,
            15.0,
            title.x + 6.0,
            cy,
            name,
            tw,
            t.text,
        );
        title.right() + 10.0
    }

    /// Small pill with a short text; true when clicked.
    #[allow(clippy::too_many_arguments)]
    fn pill(
        &mut self,
        c: &mut Canvas,
        input: &Input,
        id: u64,
        r: Rect,
        text: &str,
        on: bool,
        tip: &str,
    ) -> bool {
        let t = self.theme;
        let hov = self.hovered(input, id, r);
        let hv = self.hover_anim(id, hov);
        let onv = self.anim.to(id ^ 0x4f4e, on as u8 as f32, T_HOVER);
        c.fill_rrect(
            r,
            8.0,
            t.raised.mix(t.raised_hi, hv).mix(t.accent_deep, onv * 0.6),
        );
        c.stroke_rrect(r, 8.0, 1.0, t.hairline.mix(t.accent, onv * 0.5));
        let (cx, cy) = r.center();
        self.text_center(
            c,
            self.f.mono_med,
            12.0,
            cx,
            cy,
            text,
            t.text2.mix(t.text, hv).mix(t.accent, onv),
        );
        if hov {
            self.set_tip(tip, "");
            self.set_cursor(Cursor::Hand);
        }
        hov && input.pressed
    }

    /// Row of color swatches; returns the color picked (0: the default, by
    /// clicking the current one again).
    pub(super) fn swatches(
        &mut self,
        c: &mut Canvas,
        input: &Input,
        tag: &str,
        x: f32,
        cy: f32,
        current: u32,
    ) -> Option<u32> {
        let t = self.theme;
        let mut picked = None;
        for (k, (name, hex)) in PATTERN_COLORS.iter().enumerate() {
            let cx = x + 7.0 + k as f32 * 19.0;
            let id = wid(tag, k, 0);
            let hit = Rect::new(cx - 9.0, cy - 9.0, 18.0, 18.0);
            let hov = self.hovered(input, id, hit);
            let hv = self.hover_anim(id, hov);
            let on = current == *hex;
            c.fill_circle(cx, cy, 6.0 + hv * 1.5, Color::hex(*hex));
            if on {
                c.stroke_circle(cx, cy, 9.0, 1.5, t.text);
            }
            if hov {
                self.set_tip(tr(name), "");
                self.set_cursor(Cursor::Hand);
                if input.pressed {
                    picked = Some(if on { 0 } else { *hex });
                }
            }
        }
        picked
    }

    fn audio_clip_panel(
        &mut self,
        c: &mut Canvas,
        input: &Input,
        header: Rect,
        body: Rect,
        i: usize,
    ) {
        let t = self.theme;
        let clip = self.project.audio_clips[i].clone();
        let color = audio_color(&clip, &t);
        let name = std::path::Path::new(&clip.path)
            .file_stem()
            .map_or(String::new(), |n| n.to_string_lossy().into_owned());
        let mut x = self.clip_title(c, input, header, color, &name, None);
        // Chips: what the clip does, at a glance.
        let cy = header.y + 14.0;
        let chips: Vec<(Icon, String)> = [
            clip.warp.then(|| {
                (
                    Icon::Stretch,
                    format!(
                        "{} · {} BPM",
                        mode_label(clip.warp_mode),
                        ponzik_i18n::num(clip.warp_bpm(), 1)
                    ),
                )
            }),
            clip.reverse.then(|| (Icon::Swap, String::new())),
            clip.looped.then(|| (Icon::Loop, String::new())),
        ]
        .into_iter()
        .flatten()
        .collect();
        for (icon, text) in chips {
            let w = if text.is_empty() {
                26.0
            } else {
                34.0 + self.fonts.measure(self.f.sans, 12.0, &text)
            };
            let chip = Rect::new(x, header.y + 3.0, w, 22.0);
            c.fill_rrect(chip, 11.0, color.mix(t.panel, 0.8));
            draw_icon(c, icon, chip.x + 13.0, cy, 12.0, color.mix(t.text, 0.4));
            if !text.is_empty() {
                self.text(c, self.f.sans, 12.0, chip.x + 25.0, cy, &text, t.text2);
            }
            x = chip.right() + 6.0;
        }
        if self.warping.is_pending(i) {
            let pulse = 0.5 + 0.5 * (self.time * 6.0).sin();
            c.fill_circle(x + 8.0, cy, 4.0, t.accent.alpha(0.4 + 0.6 * pulse));
            self.tip_zone(
                input,
                Rect::new(x, header.y, 16.0, 28.0),
                tr("Stretching in the background · playing resampled meanwhile"),
                "",
            );
        }

        // Knobs: gain, transpose, fine.
        let ctl = Rect::new(body.x, body.y, 236.0, body.h);
        let cell = |k: usize| Rect::new(ctl.x + k as f32 * 78.0, ctl.y, 76.0, 80.0);
        if let Some(db) = self.knob(
            c,
            input,
            wid("cp-gain", 0, 0),
            cell(0),
            &GAIN,
            gain_to_db(clip.gain),
        ) {
            let g = db_to_gain(db);
            self.edit_audio_clip(i, tr("Clip gain"), |c| c.gain = g);
        }
        self.tip_zone(input, cell(0), tr("Clip gain\nDouble-click: 0 dB"), "");
        let can_pitch = clip.warp && clip.warp_mode != WarpAlgo::RePitch;
        let pitch_tip = if can_pitch {
            tr("Transposition, the tempo unchanged\nDouble-click: none")
        } else if clip.warp {
            tr("In Re-Pitch the pitch follows the tempo")
        } else {
            tr("Turn warp on to transpose")
        };
        if let Some(v) = self.knob(
            c,
            input,
            wid("cp-transpose", 0, 0),
            cell(1),
            &TRANSPOSE,
            clip.transpose,
        ) {
            let v = v.round();
            self.edit_audio_clip(i, tr("Transpose clip"), |c| c.transpose = v);
        }
        self.tip_zone(input, cell(1), pitch_tip, "");
        if let Some(v) = self.knob(c, input, wid("cp-fine", 0, 0), cell(2), &FINE, clip.fine) {
            self.edit_audio_clip(i, tr("Transpose clip"), |c| c.fine = v.round());
        }
        self.tip_zone(input, cell(2), pitch_tip, "");
        if !can_pitch {
            c.fill_rect(
                Rect::new(cell(1).x, cell(1).y, 154.0, 80.0),
                t.bg_deep.alpha(0.55),
            );
        }

        // Toggles: warp, reverse, loop.
        let ty = ctl.y + 92.0;
        let b = |k: usize| Rect::new(ctl.x + k as f32 * 44.0, ty, 38.0, 38.0);
        if self.icon_button(
            c,
            input,
            wid("cp-warp", 0, 0),
            b(0),
            Icon::Stretch,
            clip.warp,
            (tr("Warp: follow the song tempo"), ""),
        ) {
            self.toggle_warp(i);
        }
        if self.icon_button(
            c,
            input,
            wid("cp-rev", 0, 0),
            b(1),
            Icon::Swap,
            clip.reverse,
            (tr("Reverse"), ""),
        ) {
            self.edit_audio_clip(i, tr("Reverse clip"), |c| c.reverse = !c.reverse);
        }
        if self.icon_button(
            c,
            input,
            wid("cp-loop", 0, 0),
            b(2),
            Icon::Loop,
            clip.looped,
            (tr("Loop: the file starts over"), ""),
        ) {
            self.edit_audio_clip(i, tr("Loop clip"), |c| c.looped = !c.looped);
        }
        if self.icon_button(
            c,
            input,
            wid("cp-lab", 0, 0),
            b(3),
            Icon::Lab,
            false,
            (tr("Open in the Sample Lab"), ""),
        ) {
            self.open_in_lab(&clip.path, super::samplelab::Source::Clip(i));
            return;
        }
        if self.icon_button(
            c,
            input,
            wid("cp-edit", 0, 0),
            b(4),
            Icon::Pencil,
            false,
            (tr("Edit the audio: cut, clean, stretch, on a copy"), ""),
        ) {
            self.open_in_editor(&clip.path, super::samplelab::Source::Clip(i));
            return;
        }
        if let Some(hex) =
            self.swatches(c, input, "cp-color", ctl.x, ctl.bottom() - 12.0, clip.color)
        {
            self.edit_audio_clip(i, tr("Clip color"), |c| c.color = hex);
        }

        // Warp: mode, original tempo.
        let wc = Rect::new(ctl.right() + 14.0, body.y, 210.0, body.h);
        let dim = if clip.warp { 1.0 } else { 0.45 };
        let modes = [
            mode_label(WarpAlgo::Beats),
            mode_label(WarpAlgo::Tones),
            mode_label(WarpAlgo::RePitch),
        ];
        let tips = [
            (tr("Beats: hits stay sharp, for drums and loops"), ""),
            (tr("Tones: smooth, for chords, pads and voices"), ""),
            (
                tr("Re-Pitch: the pitch follows the tempo, like a turntable"),
                "",
            ),
        ];
        let sel = WarpAlgo::ALL
            .iter()
            .position(|&m| m == clip.warp_mode)
            .unwrap_or(0);
        if let Some(k) = self.segmented_tips(
            c,
            input,
            wid("cp-mode", 0, 0),
            Rect::new(wc.x, wc.y + 4.0, wc.w, 32.0),
            &modes,
            &tips,
            sel,
        ) {
            let mode = WarpAlgo::ALL[k];
            if clip.warp {
                self.edit_audio_clip(i, tr("Warp mode"), |c| c.warp_mode = mode);
            } else {
                self.toggle_warp(i);
                self.edit_audio_clip(i, tr("Warp mode"), |c| c.warp_mode = mode);
            }
        }
        let bpm_r = Rect::new(wc.x, wc.y + 46.0, wc.w, 40.0);
        let bpm_text = ponzik_i18n::num(clip.warp_bpm(), 2);
        if let Some(v) = self.drag_field(
            c,
            input,
            wid("cp-bpm", 0, 0),
            bpm_r,
            clip.warp_bpm() as f32,
            0.1,
            1.0,
            &bpm_text,
            "BPM",
        ) {
            let v = (v * 100.0).round() / 100.0;
            let v = v.clamp(20.0, 999.0);
            self.edit_audio_clip(i, tr("Original tempo"), |c| c.bpm = v);
        }
        self.tip_zone(
            input,
            bpm_r,
            tr("Original tempo of the file\nDrag or scroll · Shift: fine"),
            "",
        );
        let row = bpm_r.bottom() + 8.0;
        let half = Rect::new(wc.x, row, 44.0, 30.0);
        if self.pill(
            c,
            input,
            wid("cp-half", 0, 0),
            half,
            "÷2",
            false,
            tr("Half the original tempo"),
        ) {
            self.edit_audio_clip(i, tr("Original tempo"), |c| {
                c.bpm = (c.warp_bpm() as f32 / 2.0).max(20.0)
            });
        }
        if self.pill(
            c,
            input,
            wid("cp-double", 0, 0),
            half.translate(50.0, 0.0),
            "×2",
            false,
            tr("Double the original tempo"),
        ) {
            self.edit_audio_clip(i, tr("Original tempo"), |c| {
                c.bpm = (c.warp_bpm() as f32 * 2.0).min(999.0)
            });
        }
        let detect = Rect::new(wc.x + 100.0, row, wc.w - 100.0, 30.0);
        if self.pill(
            c,
            input,
            wid("cp-detect", 0, 0),
            detect,
            "",
            false,
            tr("Detect the original tempo"),
        ) {
            match self.detect_clip_tempo(i) {
                Some(b) => {
                    self.edit_audio_clip(i, tr("Detect tempo"), |c| c.bpm = b);
                    self.toast(&tr_fmt!(
                        "Tempo found: {} BPM",
                        ponzik_i18n::num(b as f64, 2)
                    ));
                }
                None => self.toast(tr("No clear tempo in this file")),
            }
        }
        let (dx, dy) = detect.center();
        draw_icon(c, Icon::Metronome, dx - 30.0, dy, 14.0, t.text2);
        self.text(
            c,
            self.f.sans_med,
            12.0,
            dx - 18.0,
            dy,
            tr("Detect"),
            t.text2,
        );
        // Stretch ratio: file tempo → song tempo.
        let song_bpm = self.project.tempo_map().bpm_at(clip.start as f64);
        let ratio_y = row + 46.0;
        self.text(
            c,
            self.f.mono,
            12.0,
            wc.x,
            ratio_y,
            &ponzik_i18n::num(clip.warp_bpm(), 1),
            t.muted,
        );
        let w0 = self
            .fonts
            .measure(self.f.mono, 12.0, &ponzik_i18n::num(clip.warp_bpm(), 1));
        draw_icon(
            c,
            Icon::ChevronRight,
            wc.x + w0 + 10.0,
            ratio_y,
            11.0,
            t.muted,
        );
        self.text(
            c,
            self.f.mono_med,
            12.0,
            wc.x + w0 + 20.0,
            ratio_y,
            &format!("{} BPM", ponzik_i18n::num(song_bpm, 1)),
            t.text2,
        );
        if !clip.warp_markers.is_empty() {
            let reset = Rect::new(wc.right() - 30.0, ratio_y - 15.0, 30.0, 30.0);
            if self.icon_button(
                c,
                input,
                wid("cp-clear-markers", 0, 0),
                reset,
                Icon::Reset,
                false,
                (tr("Remove the warp markers"), ""),
            ) {
                self.edit_audio_clip(i, tr("Warp markers"), |c| c.warp_markers.clear());
            }
        }
        if dim < 1.0 {
            c.fill_rect(
                Rect::new(wc.x - 2.0, wc.y, wc.w + 4.0, ratio_y + 16.0 - wc.y),
                t.bg_deep.alpha(0.45),
            );
        }

        let wave = Rect::new(
            wc.right() + 18.0,
            body.y,
            body.right() - wc.right() - 18.0,
            body.h,
        );
        if wave.w > 80.0 {
            self.clip_wave_editor(c, input, wave, i);
        }
    }

    /// Big waveform of an audio clip: beat grid, fades and their curves,
    /// warp markers, transients and playhead.
    fn clip_wave_editor(&mut self, c: &mut Canvas, input: &Input, r: Rect, i: usize) {
        let t = self.theme;
        let clip = self.project.audio_clips[i].clone();
        let color = audio_color(&clip, &t);
        let map = self.project.tempo_map();
        let duration = self
            .samples
            .get(&clip.path)
            .map_or(0.0, |s| s.duration() as f64);
        let sr = self
            .samples
            .get(&clip.path)
            .map_or(48_000.0, |s| s.sample_rate as f64);
        c.fill_rrect(r, 12.0, t.well);
        let prev = c.push_clip(r);
        let wave = Rect::new(r.x + 8.0, r.y + STRIP + 4.0, r.w - 16.0, r.h - STRIP - 10.0);
        let len = clip.len.max(1) as f64;
        let x_of = |tick: f64| wave.x + (tick / len) as f32 * wave.w;
        let tick_at = |x: f32| ((x - wave.x) / wave.w) as f64 * len;
        let start = clip.start as f64;

        // Beat grid of the song under the clip, bars stronger.
        map.for_each_beat(start, start + len, |tick, down| {
            let x = x_of(tick - start).round();
            c.fill_rect(
                Rect::new(x, r.y + 4.0, 1.0, r.h - 8.0),
                t.hairline.alpha(if down { 2.4 } else { 0.9 }),
            );
        });

        // Waveform, through warp, loop and reverse.
        let mid = wave.y + wave.h * 0.5;
        let amp = wave.h * 0.46;
        if let Some(peaks) = self.waveform(&clip.path) {
            let bucket = super::app::WAVE_BUCKET as f64;
            for px in wave.x as i32..wave.right() as i32 {
                let (ta, tb) = (tick_at(px as f32), tick_at(px as f32 + 1.0));
                let (Some(a), Some(b)) = (
                    self.clip_file_secs(&clip, start + ta, &map, duration),
                    self.clip_file_secs(&clip, start + tb, &map, duration),
                ) else {
                    continue;
                };
                let (a, b) = (
                    (a.min(b) * sr / bucket) as usize,
                    (a.max(b) * sr / bucket) as usize,
                );
                let (mut lo, mut hi) = (0.0f32, 0.0f32);
                for k in a..=b.min(a + 64) {
                    if let Some(&(l, h)) = peaks.get(k) {
                        lo = lo.min(l);
                        hi = hi.max(h);
                    }
                }
                if hi > lo {
                    let g = clip.gain.min(2.0);
                    c.fill_rect(
                        Rect::new(
                            px as f32,
                            mid - hi * g * amp,
                            1.0,
                            ((hi - lo) * g * amp).max(1.0),
                        ),
                        color.mix(t.text, 0.35).alpha(0.85),
                    );
                }
            }
        }
        c.fill_rect(Rect::new(wave.x, mid, wave.w, 1.0), t.hairline);

        // Fades: shaded outside the gain curve, a handle for the length and one for the curve.
        let secs0 = map.secs_at(start);
        let clip_secs = map.secs_at(start + len) - secs0;
        let tick_of_secs = |s: f64| map.tick_at_secs(secs0 + s) - start;
        let fade_in_x = x_of(tick_of_secs(clip.fade_in as f64));
        let fade_out_x = x_of(tick_of_secs(clip_secs - clip.fade_out as f64));
        let top = wave.y;
        let curve_y = |g: f64| top + wave.h * (1.0 - g as f32);
        for px in wave.x as i32..wave.right() as i32 {
            let x = px as f32;
            let g_in = if x < fade_in_x {
                ponzik_engine::fade_shape(
                    ((x - wave.x) / (fade_in_x - wave.x).max(1.0)) as f64,
                    clip.fade_in_curve,
                )
            } else {
                1.0
            };
            let g_out = if x > fade_out_x {
                ponzik_engine::fade_shape(
                    ((wave.right() - x) / (wave.right() - fade_out_x).max(1.0)) as f64,
                    clip.fade_out_curve,
                )
            } else {
                1.0
            };
            let g = g_in.min(g_out);
            if g < 0.999 {
                c.fill_rect(
                    Rect::new(x, top, 1.0, curve_y(g) - top),
                    t.bg_deep.alpha(0.6),
                );
            }
        }
        let mut line = |x0: f32, x1: f32, curve: f32, rising: bool| {
            let pts: Vec<(f32, f32)> = (0..=24)
                .map(|k| {
                    let u = k as f32 / 24.0;
                    let x = x0 + (x1 - x0) * u;
                    let g = ponzik_engine::fade_shape(
                        if rising { u as f64 } else { 1.0 - u as f64 },
                        curve,
                    );
                    (x, curve_y(g))
                })
                .collect();
            c.stroke_polyline(&pts, 1.5, t.accent);
        };
        line(wave.x, fade_in_x, clip.fade_in_curve, true);
        line(fade_out_x, wave.right(), clip.fade_out_curve, false);
        let fades = [
            (
                0usize,
                fade_in_x,
                clip.fade_in,
                clip.fade_in_curve,
                (wave.x + fade_in_x) * 0.5,
            ),
            (
                1,
                fade_out_x,
                clip.fade_out,
                clip.fade_out_curve,
                (fade_out_x + wave.right()) * 0.5,
            ),
        ];
        for (k, hx, secs, curve, cx) in fades {
            // Length handle, at the top of the fade.
            let id = wid("cp-fade", k, 0);
            let handle = Rect::new(hx - 7.0, top - 7.0, 14.0, 14.0);
            let hov = self.hovered(input, id, handle.inset(-3.0));
            let hv = self.hover_anim(id, hov || self.is_dragging(id));
            c.fill_rrect(handle, 4.0, t.raised_hi.mix(t.accent, 0.4 + 0.6 * hv));
            c.stroke_rrect(handle, 4.0, 1.0, t.bg_deep);
            if hov {
                let label = if k == 0 {
                    tr("Fade in")
                } else {
                    tr("Fade out")
                };
                self.set_tip(
                    &format!("{label} · {} ms", ponzik_i18n::num(secs as f64 * 1000.0, 0)),
                    "",
                );
                self.set_cursor(Cursor::ResizeH);
                if input.pressed {
                    if input.double {
                        self.edit_audio_clip(i, tr("Clip fades"), |c| {
                            if k == 0 {
                                c.fade_in = MIN_FADE
                            } else {
                                c.fade_out = MIN_FADE
                            }
                        });
                    } else {
                        self.begin_drag(id, input, secs);
                    }
                }
            }
            if self.is_dragging(id) {
                self.set_cursor(Cursor::ResizeH);
                let at = map.secs_at(start + tick_at(input.x).clamp(0.0, len)) - secs0;
                let other = if k == 0 { clip.fade_out } else { clip.fade_in } as f64;
                let v = if k == 0 { at } else { clip_secs - at };
                let v = v.clamp(MIN_FADE as f64, (clip_secs - other).max(MIN_FADE as f64)) as f32;
                if (v - secs).abs() > 1e-6 {
                    self.edit_audio_clip(i, tr("Clip fades"), |c| {
                        if k == 0 {
                            c.fade_in = v
                        } else {
                            c.fade_out = v
                        }
                    });
                }
            }
            // Curve handle, on the middle of the fade.
            let span = if k == 0 {
                hx - wave.x
            } else {
                wave.right() - hx
            };
            if span < 18.0 {
                continue;
            }
            let g = ponzik_engine::fade_shape(0.5, curve);
            let (px, py) = (cx, curve_y(g));
            let id = wid("cp-curve", k, 0);
            let hov = self.hovered(input, id, Rect::new(px - 9.0, py - 9.0, 18.0, 18.0));
            let hv = self.hover_anim(id, hov || self.is_dragging(id));
            c.fill_circle(px, py, 4.5 + hv * 1.5, t.accent.mix(t.text, hv * 0.5));
            c.stroke_circle(px, py, 6.0 + hv * 1.5, 1.0, t.bg_deep);
            if hov {
                self.set_tip(
                    tr("Fade curve\nDrag up or down · double-click: straight"),
                    "",
                );
                self.set_cursor(Cursor::ResizeV);
                if input.pressed {
                    if input.double {
                        self.edit_audio_clip(i, tr("Fade curve"), |c| {
                            if k == 0 {
                                c.fade_in_curve = 0.0
                            } else {
                                c.fade_out_curve = 0.0
                            }
                        });
                    } else {
                        self.begin_drag(id, input, curve);
                    }
                }
            }
            if let Some(d) = self.drag.filter(|d| d.id == id) {
                // Up: the gain rises sooner (a negative curve).
                let v = (d.v0 - (d.y0 - input.y) * 0.012).clamp(-1.0, 1.0);
                if (v - curve).abs() > 1e-4 {
                    self.edit_audio_clip(i, tr("Fade curve"), |c| {
                        if k == 0 {
                            c.fade_in_curve = v
                        } else {
                            c.fade_out_curve = v
                        }
                    });
                }
            }
        }

        // Warp markers: pin a point of the file to a beat.
        let strip = Rect::new(r.x, r.y, r.w, STRIP + 2.0);
        if clip.warp {
            let wmap = clip.warp_map();
            let origin = wmap.beat_at(clip.offset);
            let tick_of_beat = |b: f64| (b - origin) * PPQ as f64;
            // Transients of the file, as small ticks under the strip.
            let mut hits_x: Vec<f32> = Vec::new();
            if let Some(onsets) = self.file_onsets(&clip.path) {
                for &s in onsets.iter() {
                    let s = if clip.reverse { duration - s } else { s };
                    let x = x_of(tick_of_beat(wmap.beat_at(s)));
                    if x >= wave.x && x <= wave.right() {
                        hits_x.push(x);
                        c.fill_rect(
                            Rect::new(x.round(), strip.bottom() - 6.0, 1.0, 5.0),
                            t.text2.alpha(0.5),
                        );
                    }
                }
            }
            c.fill_rect(Rect::new(r.x, strip.bottom(), r.w, 1.0), t.hairline);
            let markers = clip.warp_markers.clone();
            let mut grabbed = None;
            for (k, m) in markers.iter().enumerate() {
                let x = x_of(tick_of_beat(m.beat));
                if x < r.x - 8.0 || x > r.right() + 8.0 {
                    continue;
                }
                let id = wid("cp-marker", i, k);
                let hov = self.hovered(input, id, Rect::new(x - 7.0, strip.y, 14.0, strip.h));
                let hv = self.hover_anim(id, hov || self.is_dragging(id));
                c.fill_rect(
                    Rect::new(
                        x.round() - 0.5,
                        strip.bottom(),
                        1.5,
                        r.bottom() - strip.bottom() - 4.0,
                    ),
                    t.accent.alpha(0.55 + 0.45 * hv),
                );
                let mut tri = Path::new();
                let s = 6.0 + hv * 1.5;
                tri.polygon(&[
                    (x - s, strip.y + 4.0),
                    (x + s, strip.y + 4.0),
                    (x, strip.y + 4.0 + 1.6 * s),
                ]);
                c.fill_path(&tri, t.accent.mix(t.text, hv * 0.4));
                if hov {
                    self.set_tip(tr("Warp marker\nDrag: move this point of the audio to another beat · right-click: remove"), "");
                    self.set_cursor(Cursor::ResizeH);
                    if input.pressed {
                        self.begin_drag(id, input, 0.0);
                        self.clip_grab = (input.x, m.beat);
                    }
                    if input.right_pressed {
                        self.edit_audio_clip(i, tr("Warp markers"), |c| {
                            c.warp_markers.retain(|w| w != m);
                        });
                        grabbed = Some(usize::MAX);
                    }
                }
                if self.is_dragging(id) {
                    grabbed = Some(k);
                }
            }
            if let Some(k) = grabbed.filter(|&k| k < markers.len()) {
                let (x0, b0) = self.clip_grab;
                let raw = b0 + (tick_at(input.x) - tick_at(x0)) / PPQ as f64;
                // Snapped to a sixteenth (Shift: free); never past its neighbours.
                let beat = if input.mods.shift {
                    raw
                } else {
                    (raw * 4.0).round() / 4.0
                };
                let lo = if k > 0 {
                    markers[k - 1].beat + 1.0 / 64.0
                } else {
                    f64::MIN
                };
                let hi = markers.get(k + 1).map_or(f64::MAX, |n| n.beat - 1.0 / 64.0);
                let beat = beat.clamp(lo, hi);
                if (beat - markers[k].beat).abs() > 1e-9 {
                    self.edit_audio_clip(i, tr("Warp markers"), |c| {
                        if let Some(m) = c.warp_markers.get_mut(k) {
                            m.beat = beat;
                        }
                    });
                }
            }
            // Double-click: a marker here (on a transient when one is close).
            let zone = Rect::new(wave.x, strip.y, wave.w, r.h);
            let id = wid("cp-marker-add", i, 0);
            if grabbed.is_none() && self.hovered(input, id, zone) {
                if input.y < strip.bottom() {
                    self.set_tip(tr("Double-click: warp marker (on the nearest hit)"), "");
                }
                if input.pressed && input.double {
                    let x = hits_x
                        .iter()
                        .copied()
                        .filter(|h| (h - input.x).abs() < 8.0)
                        .min_by(|a, b| (a - input.x).abs().total_cmp(&(b - input.x).abs()))
                        .unwrap_or(input.x);
                    let beat = origin + tick_at(x) / PPQ as f64;
                    let secs = wmap.secs_at(beat);
                    let anchor = WarpMarker {
                        secs: clip.offset,
                        beat: origin,
                    };
                    self.edit_audio_clip(i, tr("Warp markers"), |c| {
                        // The first marker comes with an anchor on the clip start: moving it stretches, not shifts.
                        if c.warp_markers.is_empty() && (secs - anchor.secs).abs() > 1e-3 {
                            c.warp_markers.push(anchor);
                        }
                        c.warp_markers.push(WarpMarker { secs, beat });
                        c.warp_markers.sort_by(|a, b| a.secs.total_cmp(&b.secs));
                        c.warp_markers = ponzik_project::WarpMap::new(
                            120.0,
                            std::mem::take(&mut c.warp_markers),
                        )
                        .markers()
                        .to_vec();
                    });
                }
            }
        } else {
            self.tip_zone(input, strip, tr("Turn warp on to place warp markers"), "");
        }

        // Playhead.
        let pos = self.position - start;
        if self.project.mode == ponzik_engine::PlayMode::Song && (0.0..len).contains(&pos) {
            let x = x_of(pos);
            c.fill_rect(
                Rect::new(x.round() - 1.0, r.y, 2.0, r.h),
                t.select.alpha(0.9),
            );
        }
        c.set_clip(Some(prev));
        c.stroke_rrect(r, 12.0, 1.0, t.hairline);
    }

    fn pattern_clip_panel(
        &mut self,
        c: &mut Canvas,
        input: &Input,
        header: Rect,
        body: Rect,
        i: usize,
    ) {
        let t = self.theme;
        let clip = self.project.clips[i];
        let Some(pattern) = self.project.patterns.get(clip.pattern).cloned() else {
            return;
        };
        let color = self.pattern_tint(clip.pattern);
        self.clip_title(
            c,
            input,
            header,
            color,
            &pattern.name,
            Some(Rename::Pattern(clip.pattern)),
        );

        let ctl = Rect::new(body.x, body.y, 236.0, body.h);
        self.text(
            c,
            self.f.sans,
            12.0,
            ctl.x,
            ctl.y + 8.0,
            tr("Color"),
            t.muted,
        );
        if let Some(hex) = self.swatches(c, input, "cp-pcolor", ctl.x, ctl.y + 30.0, pattern.color)
        {
            if let Some(p) = self.project.patterns.get_mut(clip.pattern) {
                p.color = hex;
            }
            self.commit(tr("Pattern color"));
        }
        // Clips sharing the pattern: « make unique » gives this one its own copy.
        let users = self
            .project
            .clips
            .iter()
            .filter(|c| c.pattern == clip.pattern)
            .count();
        let unique = Rect::new(ctl.x, ctl.y + 56.0, 236.0, 38.0);
        let id = wid("cp-unique", 0, 0);
        let hov = self.hovered(input, id, unique) && users > 1;
        let hv = self.hover_anim(id, hov);
        let alpha = if users > 1 { 1.0 } else { 0.45 };
        c.fill_rrect(unique, 10.0, t.raised.mix(t.raised_hi, hv).alpha(alpha));
        c.stroke_rrect(unique, 10.0, 1.0, t.hairline);
        draw_icon(
            c,
            Icon::Duplicate,
            unique.x + 20.0,
            unique.y + 19.0,
            15.0,
            t.text2.mix(t.accent, hv).alpha(alpha),
        );
        self.text(
            c,
            self.f.sans_med,
            13.0,
            unique.x + 38.0,
            unique.y + 19.0,
            tr("Make unique"),
            t.text.alpha(alpha),
        );
        self.text_right(
            c,
            self.f.mono,
            12.0,
            unique.right() - 12.0,
            unique.y + 19.0,
            &format!("×{users}"),
            t.muted,
        );
        if hov {
            self.set_tip(tr("This clip gets its own copy of the pattern"), "");
            self.set_cursor(Cursor::Hand);
            if input.pressed {
                self.make_unique(i);
                return;
            }
        } else if users <= 1 {
            self.tip_zone(input, unique, tr("Only this clip plays the pattern"), "");
        }
        let edit = Rect::new(ctl.x, unique.bottom() + 8.0, 38.0, 38.0);
        if self.icon_button(
            c,
            input,
            wid("cp-edit-pat", 0, 0),
            edit,
            Icon::Rack,
            false,
            (tr("Edit"), ""),
        ) {
            self.select_pattern(clip.pattern);
            self.set_view(View::Rack);
            return;
        }

        // Notes of the clip, big.
        let r = Rect::new(
            ctl.right() + 18.0,
            body.y,
            body.right() - ctl.right() - 18.0,
            body.h,
        );
        if r.w < 80.0 {
            return;
        }
        c.fill_rrect(r, 12.0, t.well);
        let prev = c.push_clip(r);
        let len = clip.len.max(1) as f32;
        let x_of = |tick: f32| r.x + 8.0 + tick / len * (r.w - 16.0);
        let map = self.project.tempo_map();
        map.for_each_beat(
            clip.start as f64,
            (clip.start + clip.len) as f64,
            |tick, down| {
                let x = x_of(tick as f32 - clip.start as f32).round();
                c.fill_rect(
                    Rect::new(x, r.y + 4.0, 1.0, r.h - 8.0),
                    t.hairline.alpha(if down { 2.4 } else { 0.9 }),
                );
            },
        );
        let all: Vec<_> = pattern.notes.iter().flatten().collect();
        if all.is_empty() {
            let (cx, cy) = r.center();
            self.text_center(c, self.f.sans, 13.0, cx, cy, tr("Empty pattern"), t.muted);
        } else {
            let (lo, hi) = all
                .iter()
                .fold((127u8, 0u8), |(lo, hi), n| (lo.min(n.key), hi.max(n.key)));
            let span = (hi - lo).max(8) as f32 + 1.0;
            let row = (r.h - 20.0) / span;
            // Few keys: centred.
            let pad = (span - (hi - lo) as f32 - 1.0) * 0.5;
            let plen = pattern.len.max(1);
            let mut rep = 0;
            while rep * plen < clip.offset + clip.len {
                for n in &all {
                    let s = rep * plen + n.start;
                    if s < clip.offset || s - clip.offset >= clip.len {
                        continue;
                    }
                    let x = x_of((s - clip.offset) as f32);
                    let y = r.bottom() - 10.0 - ((n.key - lo + 1) as f32 + pad) * row;
                    let w = (n.len as f32 / len * (r.w - 16.0)).max(2.0);
                    c.fill_rrect(
                        Rect::new(x, y, w, row.clamp(2.0, 10.0)),
                        2.0,
                        color.mix(t.text, 0.2).alpha(0.4 + 0.6 * n.vel),
                    );
                }
                rep += 1;
            }
        }
        c.set_clip(Some(prev));
        c.stroke_rrect(r, 12.0, 1.0, t.hairline);
    }

    /// Screenshot setup (`PONZIK_CLIP=audio[,markers|tones|off]|pattern`): a
    /// synthetic drum loop warped in the playlist, its clip panel open.
    pub fn screenshot_clip(&mut self, spec: &str) {
        if spec.starts_with("pattern") {
            if !self.project.clips.is_empty() {
                self.playlist.selected = Some(0);
                self.select_clip(ClipSel::Pattern(0));
            }
            return;
        }
        // Two bars of a 96 BPM beat: kick, snare, hats, a bass note.
        let sr = 48_000.0f32;
        let beat = 60.0 / 96.0;
        let len = (8.0 * beat * sr) as usize;
        let mut left = vec![0.0f32; len];
        let mut seed = 7u32;
        let mut noise = move || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 9) as f32 / (1u32 << 23) as f32 * 2.0 - 1.0
        };
        for k in 0..16 {
            let at = (k as f32 * beat * 0.5 * sr) as usize;
            let (amp, decay, kick) = match k % 4 {
                0 => (0.9, 5_000.0, true),
                2 => (0.6, 3_000.0, false),
                _ => (0.18, 800.0, false),
            };
            for j in 0..(len - at).min(14_000) {
                let env = (-(j as f32) / decay).exp();
                let s = if kick {
                    (j as f32 * 55.0 * std::f32::consts::TAU / sr
                        * (1.0 + 2.0 * (-(j as f32) / 900.0).exp()))
                    .sin()
                } else {
                    noise()
                };
                left[at + j] += amp * env * s;
            }
        }
        let path = "Ponzik loop 96.wav".to_string();
        let sample = ponzik_engine::Sample {
            name: path.clone(),
            sample_rate: sr,
            right: left.clone(),
            left,
            markers: Vec::new(),
        };
        self.samples.insert(path.clone(), Arc::new(sample));
        let lane = self
            .project
            .clips
            .iter()
            .map(|c| c.lane + 1)
            .max()
            .unwrap_or(0);
        let mut clip = AudioClipDef {
            path,
            lane,
            track: 9,
            start: 0,
            len: 16 * PPQ,
            warp: !spec.contains("off"),
            bpm: 96.0,
            looped: true,
            fade_in: 0.05,
            fade_out: 0.4,
            fade_out_curve: 0.6,
            color: 0xf0b44a,
            ..Default::default()
        };
        if spec.contains("tones") {
            clip.warp_mode = WarpAlgo::Tones;
            clip.transpose = 3.0;
        }
        if spec.contains("markers") {
            clip.warp_markers = vec![
                WarpMarker {
                    secs: 0.0,
                    beat: 0.0,
                },
                WarpMarker {
                    secs: 2.5,
                    beat: 4.5,
                },
            ];
        }
        self.project.audio_clips.push(clip);
        self.commit(tr("Place an audio clip"));
        let i = self.project.audio_clips.len() - 1;
        self.focus = Focus::Track(9);
        self.select_clip(ClipSel::Audio(i));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        let project = ponzik_project::demo_project();
        let none = |_: &str| None;
        let (_engine, handle) = project.build_engine(48_000.0, &none);
        let mut app = App::new(handle, project, 48_000);
        app.set_view(View::Playlist);
        app.set_mode(ponzik_engine::PlayMode::Song);
        app
    }

    fn step(app: &mut App, c: &mut Canvas, input: &mut Input, x: f32, y: f32, down: bool) {
        input.pressed = down && !input.down;
        input.released = !down && input.down;
        (input.x, input.y, input.inside, input.down) = (x, y, true, down);
        app.frame(c, input, 1.0 / 60.0);
    }

    fn click(app: &mut App, c: &mut Canvas, input: &mut Input, x: f32, y: f32) {
        step(app, c, input, x, y, false);
        step(app, c, input, x, y, true);
        step(app, c, input, x, y, false);
    }

    #[test]
    fn clip_panel_edits_the_selected_clip() {
        let mut app = app();
        let mut c = Canvas::new(1600, 960);
        let mut i = Input::default();
        app.screenshot_clip("audio");
        let n = app.project.audio_clips.len() - 1;
        step(&mut app, &mut c, &mut i, 800.0, 400.0, false);
        assert_eq!(app.clip_panel_shown(), Some(ClipSel::Audio(n)));
        // Warp off, then undone.
        click(&mut app, &mut c, &mut i, 283.0, 848.0);
        assert!(!app.project.audio_clips[n].warp);
        app.undo();
        assert!(app.project.audio_clips[n].warp);
        // Reverse and loop.
        click(&mut app, &mut c, &mut i, 327.0, 848.0);
        assert!(app.project.audio_clips[n].reverse);
        // Tones mode, then ×2 on the original tempo.
        click(&mut app, &mut c, &mut i, 619.0, 757.0);
        assert_eq!(app.project.audio_clips[n].warp_mode, WarpAlgo::Tones);
        click(&mut app, &mut c, &mut i, 585.0, 845.0);
        assert_eq!(app.project.audio_clips[n].bpm, 192.0);
        // The fade-in handle, dragged right.
        let before = app.project.audio_clips[n].fade_in;
        step(&mut app, &mut c, &mut i, 755.0, 761.0, false);
        step(&mut app, &mut c, &mut i, 755.0, 761.0, true);
        step(&mut app, &mut c, &mut i, 900.0, 761.0, true);
        step(&mut app, &mut c, &mut i, 900.0, 761.0, false);
        assert!(
            app.project.audio_clips[n].fade_in > before + 0.2,
            "{}",
            app.project.audio_clips[n].fade_in
        );
        // Double-click in the marker strip: a warp marker (and its anchor).
        step(&mut app, &mut c, &mut i, 1200.0, 748.0, false);
        i.double = true;
        step(&mut app, &mut c, &mut i, 1200.0, 748.0, true);
        i.double = false;
        step(&mut app, &mut c, &mut i, 1200.0, 748.0, false);
        assert_eq!(app.project.audio_clips[n].warp_markers.len(), 2);
        // The close button brings the devices back.
        click(&mut app, &mut c, &mut i, 1569.0, 712.0);
        assert_eq!(app.clip_panel_shown(), None);
    }

    #[test]
    fn warp_renders_land_in_the_background() {
        let mut app = app();
        app.screenshot_clip("audio,tones");
        let n = app.project.audio_clips.len() - 1;
        assert!(app.warping.is_pending(n), "a render is queued");
        let t0 = std::time::Instant::now();
        while app.warping.is_pending(n) && t0.elapsed().as_secs() < 30 {
            std::thread::sleep(std::time::Duration::from_millis(20));
            app.poll_warps();
        }
        assert!(!app.warping.is_pending(n));
        let key = app.warping.keys[n].expect("a render key");
        assert!(warp::cached(key).is_some());
    }
}
