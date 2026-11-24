//! Couloirs d'automation dans la playlist : une courbe par paramètre automatisé.

use ponzik_engine::{AutoPoint, AutoScope, AutoTarget, ParamInfo, PlayMode, Unit};
use ponzik_i18n::{tr, tr_fmt, tr_noop};
use ponzik_ui::{Canvas, Color, Path, Rect};

use super::Input;
use super::app::{Action, App, Focus, MenuItem, View};
use super::devices::params_of;
use super::lanes::PianoLane;

use super::widgets::{Icon, wid};

/// Interface state of the curves: the one just jumped to blinks.
#[derive(Default)]
pub struct AutoUi {
    /// Curve shown by "Show the curve", and when.
    pub flash: Option<(AutoTarget, AutoScope, f32)>,
    /// Scroll of the automation list, in pixels.
    pub list_scroll: f32,
}

/// How long a curve jumped to stays highlighted, in seconds.
const FLASH_SECS: f32 = 1.6;

pub const AUTO_LANE_H: f32 = 56.0;
const POINT_HIT: f32 = 8.0;

/// Name of a MIDI control change: the common ones by name, else "CC n".
pub fn cc_name(cc: u8) -> String {
    match cc {
        1 => tr("Mod wheel").into(),
        2 => tr("Breath").into(),
        7 => tr("Volume").into(),
        10 => tr("Pan").into(),
        11 => tr("Expression").into(),
        64 => tr("Sustain pedal").into(),
        74 => tr("Brightness").into(),
        _ => format!("CC {cc}"),
    }
}

impl App {
    /// Nom lisible et fiche de paramètre d'une cible d'automation.
    pub fn target_info(&self, target: AutoTarget) -> Option<(String, ParamInfo)> {
        let p = &self.project;
        Some(match target {
            AutoTarget::InstrumentParam { channel, id } => {
                let ch = p.channels.get(channel)?;
                let info = *params_of(&ch.instrument.kind, true).iter().find(|i| i.id == id)?;
                (format!("{} · {}", tr(info.name), ch.name), info)
            }
            AutoTarget::EffectParam { track, slot, id } => {
                let fx = p.tracks.get(track)?.inserts.get(slot)?.as_ref()?;
                if let Some((unit, _)) = ponzik_engine::effects::fx_rack::split_param(id) {
                    // A device inside a rack: its name too.
                    let info = fx.param_info(id)?;
                    let kind = &fx.rack.as_deref()?.device(unit)?.kind;
                    let device = ponzik_engine::registry::effect_kind(kind).map_or(kind.as_str(), |k| tr(k.name));
                    return Some((format!("{} · {} · {}", tr(info.name), device, p.tracks[track].name), info));
                }
                let info = *params_of(&fx.kind, false).iter().find(|i| i.id == id)?;
                (format!("{} · {}", tr(info.name), p.tracks[track].name), info)
            }
            AutoTarget::ChannelGain(c) => {
                (tr_fmt!("Volume · {}", p.channels.get(c)?.name), ParamInfo::new(0, tr_noop("Volume"), 0.0, 1.25, 0.8, Unit::None))
            }
            AutoTarget::ChannelPan(c) => {
                (tr_fmt!("Pan · {}", p.channels.get(c)?.name), ParamInfo::new(0, tr_noop("Pan"), -1.0, 1.0, 0.0, Unit::Pan))
            }
            AutoTarget::TrackGain(t) => {
                (tr_fmt!("Volume · {}", p.tracks.get(t)?.name), ParamInfo::new(0, tr_noop("Volume"), 0.0, 2.0, 1.0, Unit::None))
            }
            AutoTarget::TrackPan(t) => {
                (tr_fmt!("Pan · {}", p.tracks.get(t)?.name), ParamInfo::new(0, tr_noop("Pan"), -1.0, 1.0, 0.0, Unit::Pan))
            }
            AutoTarget::Tempo => (tr("Tempo").into(), ParamInfo::new(0, tr_noop("Tempo"), 40.0, 300.0, p.bpm, Unit::None)),
            AutoTarget::PitchBend(c) => (
                tr_fmt!("Pitch bend · {}", p.channels.get(c)?.name),
                ParamInfo::new(0, tr_noop("Pitch bend"), -1.0, 1.0, 0.0, Unit::None),
            ),
            AutoTarget::Cc { channel, cc } => {
                let name = p.channels.get(channel)?.name.as_str();
                (format!("{} · {name}", cc_name(cc)), ParamInfo::new(0, tr_noop("Controller"), 0.0, 1.0, 0.0, Unit::Percent))
            }
        })
    }

    /// Current value of a parameter, in its own units.
    pub fn auto_value(&self, target: AutoTarget) -> Option<f32> {
        let (_, info) = self.target_info(target)?;
        let p = &self.project;
        Some(match target {
            AutoTarget::InstrumentParam { channel, id } => p.channels[channel].instrument.get(id).unwrap_or(info.default),
            AutoTarget::EffectParam { track, slot, id } => {
                p.tracks[track].inserts[slot].as_ref().and_then(|e| e.get(id)).unwrap_or(info.default)
            }
            AutoTarget::ChannelGain(c) => p.channels[c].gain,
            AutoTarget::ChannelPan(c) => p.channels[c].pan,
            AutoTarget::TrackGain(t) => p.tracks[t].gain,
            AutoTarget::TrackPan(t) => p.tracks[t].pan,
            AutoTarget::Tempo => self.tempo_here(),
            // Controllers have no value of their own outside their lanes.
            AutoTarget::PitchBend(_) | AutoTarget::Cc { .. } => info.default,
        })
    }

    /// Sets a parameter (clamped to its range); the caller commits.
    pub fn set_auto_value(&mut self, target: AutoTarget, value: f32) {
        let Some((_, info)) = self.target_info(target) else { return };
        let v = info.clamp(value);
        let p = &mut self.project;
        match target {
            AutoTarget::InstrumentParam { channel, id } => p.channels[channel].instrument.set(id, v),
            AutoTarget::EffectParam { track, slot, id } => {
                if let Some(e) = p.tracks[track].inserts[slot].as_mut() {
                    e.set(id, v);
                }
            }
            AutoTarget::ChannelGain(c) => p.channels[c].gain = v,
            AutoTarget::ChannelPan(c) => p.channels[c].pan = v,
            AutoTarget::TrackGain(t) => p.tracks[t].gain = v,
            AutoTarget::TrackPan(t) => p.tracks[t].pan = v,
            AutoTarget::Tempo => {
                self.set_tempo_here((v * 10.0).round() / 10.0);
            }
            AutoTarget::PitchBend(_) | AutoTarget::Cc { .. } => {}
        }
    }

    /// A value typed by hand (« −6 dB », « 2,5 kHz », « 50 % »…), in the unit
    /// the control shows; volumes also read decibels.
    pub fn type_value(&mut self, target: AutoTarget, text: &str) {
        let Some((label, info)) = self.target_info(target) else { return };
        let lower = text.to_lowercase();
        let gain = matches!(target, AutoTarget::ChannelGain(_) | AutoTarget::TrackGain(_));
        let value = if gain && lower.contains("db") {
            let db = ParamInfo::new(0, "", -120.0, 12.0, 0.0, Unit::Db).parse(text);
            db.map(|d| if d <= -119.9 { 0.0 } else { 10f32.powf(d / 20.0) })
        } else {
            info.parse(text)
        };
        let Some(v) = value else {
            self.toast(&tr_fmt!("Not a value for {}: {}", label, text));
            return;
        };
        self.set_auto_value(target, v);
        self.commit(&tr_fmt!("Setting: {}", tr(info.name)));
    }

    /// Crée un couloir d'automation du morceau pour `target`, à la valeur actuelle.
    pub fn create_automation(&mut self, target: AutoTarget, value: f32) {
        if self.project.automation_lane(target, AutoScope::Song).is_some() {
            self.toast(tr("This parameter is already automated: see the playlist"));
        } else {
            let end = self.project.song_len();
            self.project.add_automation_point(target, AutoScope::Song, 0, value);
            self.project.add_automation_point(target, AutoScope::Song, end, value);
            self.commit(tr("Create an automation"));
            self.toast(tr("Automation created in the playlist (Song mode)"));
        }
        self.set_view(super::app::View::Playlist);
    }

    /// Curves drawn for `target`: where each lives and whether it is muted.
    pub fn curves_of(&self, target: AutoTarget) -> Vec<(AutoScope, bool)> {
        self.project.automation.iter().filter(|a| a.target == target && !a.points.is_empty()).map(|a| (a.scope, a.muted)).collect()
    }

    /// Where a curve lives, in a few words: "Song" or the pattern's name.
    pub fn scope_label(&self, scope: AutoScope) -> String {
        match scope {
            AutoScope::Song => tr("Song").into(),
            AutoScope::Pattern(p) => self.project.patterns.get(p).map_or_else(|| tr_fmt!("Pattern {}", p + 1), |p| p.name.clone()),
        }
    }

    /// Length the curve is drawn over, in ticks: its pattern, or the song.
    pub fn scope_len(&self, scope: AutoScope) -> u32 {
        match scope {
            AutoScope::Song => self.project.song_len().max(1),
            AutoScope::Pattern(p) => self.project.patterns.get(p).map_or(1, |p| p.len.max(1)),
        }
    }

    /// Value a playing curve gives `target` right now (the knob follows it),
    /// `None` when stopped or when no curve applies here.
    pub fn live_auto_value(&self, target: AutoTarget) -> Option<f32> {
        self.project
            .automation
            .iter()
            .filter(|a| a.target == target && !a.muted && !a.points.is_empty())
            .find_map(|a| self.live_tick(a.scope).and_then(|tick| a.value_at(tick)))
    }

    /// Where the playhead is on a curve of `scope`, in its ticks, while it plays.
    pub fn live_tick(&self, scope: AutoScope) -> Option<f64> {
        if !self.playing {
            return None;
        }
        match (scope, self.project.mode) {
            (AutoScope::Song, PlayMode::Song) => Some(self.position),
            (AutoScope::Pattern(p), PlayMode::Pattern(q)) if p == q => Some(self.cycle_position()),
            (AutoScope::Pattern(p), PlayMode::Song) => self.project.clips.iter().filter(|c| c.pattern == p && !c.muted).find_map(|c| {
                let local = self.position - c.start as f64;
                (0.0..c.len as f64).contains(&local).then_some(local + c.offset as f64)
            }),
            _ => None,
        }
    }

    /// Opens the view where a curve is drawn: a pattern curve in the piano
    /// roll, on its pattern and channel with its lane shown; a song curve in
    /// the playlist, scrolled to the automation lanes. The curve blinks.
    pub fn show_curve(&mut self, target: AutoTarget, scope: AutoScope) {
        self.auto_ui.flash = Some((target, scope, self.time));
        match scope {
            AutoScope::Pattern(p) => {
                if p != self.project.current_pattern && p < self.project.patterns.len() {
                    self.select_pattern(p);
                }
                match PianoLane::of_target(target) {
                    Some((ch, lane)) => {
                        self.focus = Focus::Channel(ch);
                        self.piano.lane = lane;
                        // Its first point in view.
                        if let Some(first) = self.project.automation_lane(target, scope).and_then(|l| l.points.first())
                            && (first.tick as f32) < self.piano.scroll_x
                        {
                            self.piano.scroll_x = (first.tick as f32 - 96.0).max(0.0);
                        }
                    }
                    None => self.toast(tr("This pattern curve has no lane in the piano roll: it plays with the pattern")),
                }
                self.set_view(View::Piano);
            }
            AutoScope::Song => {
                // Automation lanes sit under the clips: scroll to the bottom.
                self.playlist.scroll_y = f32::MAX;
                self.set_view(View::Playlist);
            }
        }
    }

    /// How strongly a curve blinks (0 to 1), for a moment after "Show the curve".
    pub fn curve_flash(&self, target: AutoTarget, scope: AutoScope) -> f32 {
        match self.auto_ui.flash {
            Some((t, s, at)) if t == target && s == scope => {
                let since = self.time - at;
                let k = (since / FLASH_SECS).clamp(0.0, 1.0);
                (1.0 - k) * (0.6 + 0.4 * (since * 9.0).cos().abs())
            }
            _ => 0.0,
        }
    }

    /// Deletes the curves of `target` (only the one of `scope` if given).
    pub fn delete_curve(&mut self, target: AutoTarget, scope: Option<AutoScope>) {
        let before = self.project.automation.len();
        self.project.automation.retain(|a| !(a.target == target && scope.is_none_or(|s| s == a.scope)));
        if self.project.automation.len() != before {
            self.commit(tr("Delete a curve"));
        }
    }

    /// Mutes or plays again the curves of `target` (only the one of `scope` if given).
    pub fn mute_curve(&mut self, target: AutoTarget, scope: Option<AutoScope>, muted: bool) {
        let mut changed = false;
        for a in self.project.automation.iter_mut().filter(|a| a.target == target && scope.is_none_or(|s| s == a.scope)) {
            changed |= a.muted != muted;
            a.muted = muted;
        }
        if changed {
            self.commit(if muted { tr("Mute a curve") } else { tr("Play a curve again") });
        }
    }

    /// Flattens the curves of `target` to its knob's value: the points stay,
    /// to be drawn again, but the sound no longer moves.
    pub fn flatten_curve(&mut self, target: AutoTarget) {
        let Some(v) = self.auto_value(target) else { return };
        let mut changed = false;
        for a in self.project.automation.iter_mut().filter(|a| a.target == target) {
            for p in &mut a.points {
                changed |= p.value != v || p.curve != 0.0;
                (p.value, p.curve) = (v, 0.0);
            }
        }
        if changed {
            self.commit(tr("Reset a curve to the knob value"));
        }
    }

    /// A curve drawn small in `r` over `len` ticks, filled from the bottom;
    /// with the playhead's dot when `now` is given.
    #[allow(clippy::too_many_arguments)]
    pub fn sparkline(&self, c: &mut Canvas, r: Rect, points: &[AutoPoint], info: &ParamInfo, len: u32, col: Color, now: Option<f64>) {
        if points.is_empty() || r.w < 4.0 || r.h < 4.0 {
            return;
        }
        let len = len.max(points.last().map_or(1, |p| p.tick)).max(1) as f32;
        let y_of = |v: f32| r.bottom() - info.to_norm(v) * r.h;
        let steps = (r.w / 2.0).max(2.0) as usize;
        let pts: Vec<(f32, f32)> = (0..=steps)
            .filter_map(|i| {
                let x = r.x + r.w * i as f32 / steps as f32;
                let tick = (len * i as f32 / steps as f32) as f64;
                ponzik_engine::sequence::automation_value(points, tick).map(|v| (x, y_of(v)))
            })
            .collect();
        let mut poly = pts.clone();
        poly.push((r.right(), r.bottom()));
        poly.push((r.x, r.bottom()));
        let mut fill = Path::new();
        fill.polygon(&poly);
        c.fill_path(&fill, col.alpha(0.18));
        c.stroke_polyline(&pts, 1.3, col);
        if let Some(tick) = now
            && let Some(v) = ponzik_engine::sequence::automation_value(points, tick)
        {
            let x = r.x + r.w * (tick as f32 / len).clamp(0.0, 1.0);
            c.fill_circle(x, y_of(v), 2.6, self.theme.text);
        }
    }

    /// The mark of an automated control at (`x`, `y`): a dot (hollow when
    /// every curve is muted); hovering `hover` shows its curves in the tooltip.
    pub fn auto_mark(&mut self, c: &mut Canvas, input: &Input, (x, y): (f32, f32), hover: Rect, target: AutoTarget) {
        let curves = self.curves_of(target);
        if curves.is_empty() {
            return;
        }
        let t = self.theme;
        let muted = curves.iter().all(|&(_, m)| m);
        let k = curves.iter().map(|&(s, _)| self.curve_flash(target, s)).fold(0.0, f32::max);
        if k > 0.0 {
            c.stroke_circle(x, y, 6.0 + 2.0 * k, 1.4, t.select.alpha(k));
        }
        if muted {
            c.stroke_circle(x, y, 3.0, 1.2, t.muted);
        } else {
            c.fill_circle(x, y, 3.5, t.select);
            c.stroke_circle(x, y, 3.5, 1.0, t.bg_deep.alpha(0.8));
        }
        if input.inside && hover.contains(input.x, input.y) && self.drag.is_none() {
            let name = self.target_info(target).map_or(String::new(), |(n, _)| n);
            let state = if muted { tr("Curve muted") } else { tr("Automated") };
            self.set_tip(&tr_fmt!("{}\n{} · right-click: show, mute or delete the curve", name, state), "");
            self.tips.curve = Some(target);
        }
    }

    /// Height the curves of `target` take under a tooltip's text (0 without any).
    pub fn tip_curves_h(&self, target: AutoTarget) -> f32 {
        let n = self.curves_of(target).len().min(3);
        if n == 0 { 0.0 } else { 4.0 + n as f32 * 52.0 }
    }

    /// Draws the curves of `target` in `r` (sized by [`Self::tip_curves_h`]).
    pub fn tip_curves(&mut self, c: &mut Canvas, r: Rect, target: AutoTarget, alpha: f32) {
        let t = self.theme;
        let Some((_, info)) = self.target_info(target) else { return };
        let lanes: Vec<(AutoScope, bool, Vec<AutoPoint>)> = self
            .project
            .automation
            .iter()
            .filter(|a| a.target == target && !a.points.is_empty())
            .take(3)
            .map(|a| (a.scope, a.muted, a.points.clone()))
            .collect();
        for (i, (scope, muted, points)) in lanes.into_iter().enumerate() {
            let row = Rect::new(r.x, r.y + 4.0 + i as f32 * 52.0, r.w, 46.0);
            c.fill_rrect(row, 6.0, t.well.alpha(alpha));
            let col = if muted { t.muted } else { self.scope_color(scope) };
            let n = ponzik_i18n::tr_fmt_n!("{} point", "{} points", points.len(), points.len());
            let label = if muted { format!("{} · {n} · {}", self.scope_label(scope), tr("muted")) } else { format!("{} · {n}", self.scope_label(scope)) };
            c.fill_circle(row.x + 10.0, row.y + 10.0, 3.0, col.alpha(alpha));
            self.text_fit(c, self.f.sans, 11.0, row.x + 18.0, row.y + 10.0, &label, row.w - 26.0, t.tooltip_text2.alpha(alpha));
            let g = Rect::new(row.x + 8.0, row.y + 19.0, row.w - 16.0, row.h - 23.0);
            let now = self.live_tick(scope);
            self.sparkline(c, g, &points, &info, self.scope_len(scope), col.alpha(alpha), now);
        }
    }

    /// Items of a parameter's menu about its curves: show each, mute, delete,
    /// back to the knob's value. Empty without curves.
    pub fn curve_menu_items(&self, target: AutoTarget) -> Vec<MenuItem> {
        let curves = self.curves_of(target);
        if curves.is_empty() {
            return Vec::new();
        }
        let mut v: Vec<MenuItem> = curves
            .iter()
            .map(|&(scope, _)| {
                let tip = match scope {
                    AutoScope::Song => tr("In the playlist, under the clips"),
                    AutoScope::Pattern(_) => tr("In the piano roll, in the lane under the notes"),
                };
                MenuItem::new(Icon::Automation, tr("Show the curve"), Action::ShowCurve(target, scope)).detail(self.scope_label(scope)).tip(tip)
            })
            .collect();
        let all_muted = curves.iter().all(|&(_, m)| m);
        let mute = if all_muted {
            MenuItem::new(Icon::Play, tr("Play the curve again"), Action::MuteCurve(target, None, false))
        } else {
            MenuItem::new(Icon::Mute, tr("Mute the curve"), Action::MuteCurve(target, None, true)).tip(tr("Keeps its points; the knob's value plays"))
        };
        v.push(mute.sep());
        v.push(MenuItem::new(Icon::Reset, tr("Reset to the knob value"), Action::FlattenCurve(target)).tip(tr("The points stay, flat at the knob's value")));
        v.push(MenuItem::new(Icon::Trash, tr("Delete the curve"), Action::DeleteCurve(target, None)));
        v
    }

    /// Color of a curve's scope: its pattern's, or the automation color for the song.
    pub fn scope_color(&self, scope: AutoScope) -> Color {
        match scope {
            AutoScope::Pattern(p) => self.pattern_tint(p),
            AutoScope::Song => self.theme.select,
        }
    }

    /// Couloirs d'automation du morceau, sous les clips. Clic : poser un point,
    /// glisser : le déplacer, clic droit maintenu : effacer, clic droit sur le nom :
    /// retirer le couloir.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn automation_lanes(
        &mut self,
        c: &mut Canvas,
        input: &Input,
        area: Rect,
        y0: f32,
        x_of: &dyn Fn(f32) -> f32,
        tick_at: &dyn Fn(f32) -> f32,
        snap: f32,
    ) {
        let t = self.theme;
        let lanes = self.song_lane_targets();
        for (k, target) in lanes.into_iter().enumerate() {
            let row = Rect::new(area.x, y0 + k as f32 * AUTO_LANE_H, area.w, AUTO_LANE_H - 4.0);
            if row.y > area.bottom() {
                break;
            }
            let Some((label, info)) = self.target_info(target) else { continue };
            c.fill_rect(row, t.panel.alpha(0.55));
            let g = row.inset_xy(0.0, 6.0);
            let y_of = |v: f32| g.bottom() - info.to_norm(v) * g.h;
            let val_at = |y: f32| info.from_norm((g.bottom() - y) / g.h);

            let points = self.project.automation_lane(target, AutoScope::Song).map(|a| a.points.clone()).unwrap_or_default();
            // Muted: grey, kept; just jumped to: outlined for a moment.
            let muted = self.project.automation_lane(target, AutoScope::Song).is_some_and(|a| a.muted);
            let ink = if muted { t.muted } else { t.select };
            let flash = self.curve_flash(target, AutoScope::Song);
            if flash > 0.0 {
                c.stroke_rrect(row, 4.0, 2.0, t.select.alpha(flash));
            }
            let label = if muted { format!("{label} · {}", tr("muted")) } else { label };
            let lane_id = wid("auto-lane", k, 0);
            if points.is_empty() {
                // Created by the pass being recorded: its name and what it holds so far.
                let tw = self.fonts.measure(self.f.sans_med, 11.0, &label) + 16.0;
                let chip = Rect::new(area.x + 6.0, row.y + 4.0, tw, 18.0);
                c.fill_rrect(chip, 6.0, t.bg_deep.alpha(0.85));
                self.text(c, self.f.sans_med, 11.0, chip.x + 8.0, chip.y + 9.0, &label, t.negative);
                self.lane_arm(c, input, k, chip.right() + 12.0, chip.y + 9.0, target);
                self.draw_pass(c, target, x_of, &y_of, area);
                continue;
            }
            // Courbe : plate avant le premier point et après le dernier.
            let mut pts = vec![(area.x, y_of(points[0].value))];
            pts.extend(points.iter().map(|p| (x_of(p.tick as f32), y_of(p.value))));
            pts.push((area.right(), y_of(points[points.len() - 1].value)));
            let mut fill = pts.clone();
            fill.push((area.right(), g.bottom()));
            fill.push((area.x, g.bottom()));
            let mut path = Path::new();
            path.polygon(&fill);
            c.fill_path(&path, ink.alpha(0.12));
            c.stroke_polyline(&pts, 1.6, ink);

            let hover_point = points.iter().position(|p| {
                (input.x - x_of(p.tick as f32)).abs() < POINT_HIT && (input.y - y_of(p.value)).abs() < POINT_HIT
            });
            let over = input.inside && row.contains(input.x, input.y) && (self.drag.is_none() || self.is_dragging(lane_id));

            // Nom du couloir, en pastille.
            let tw = self.fonts.measure(self.f.sans_med, 11.0, &label) + 16.0;
            let chip = Rect::new(area.x + 6.0, row.y + 4.0, tw, 18.0);
            c.fill_rrect(chip, 6.0, t.bg_deep.alpha(0.85));
            self.text(c, self.f.sans_med, 11.0, chip.x + 8.0, chip.y + 9.0, &label, ink);
            if self.lane_arm(c, input, k, chip.right() + 12.0, chip.y + 9.0, target) {
                continue;
            }
            let arm_r = Rect::new(chip.right() + 2.0, chip.y - 1.0, 20.0, 20.0);
            let over = over && !arm_r.contains(input.x, input.y);
            if over && input.right_pressed && chip.contains(input.x, input.y) {
                self.project.automation.retain(|a| !(a.target == target && a.scope == AutoScope::Song));
                self.commit(tr("Remove an automation"));
                continue;
            }

            if over && input.pressed && self.drag.is_none() {
                let tick = match hover_point {
                    Some(i) => points[i].tick,
                    None => {
                        let tick = ((tick_at(input.x) / snap).round() * snap).max(0.0) as u32;
                        self.project.add_automation_point(target, AutoScope::Song, tick, val_at(input.y));
                        self.commit(tr("Automation point"));
                        tick
                    }
                };
                self.begin_drag(lane_id, input, tick as f32);
            }
            if let Some(d) = self.drag.filter(|d| d.id == lane_id) {
                let old = d.v0 as u32;
                let tick = ((tick_at(input.x) / snap).round() * snap).max(0.0) as u32;
                let value = val_at(input.y);
                let curve = self.project.remove_automation_point(target, AutoScope::Song, old).map_or(0.0, |p| p.curve);
                self.project.add_automation_point(target, AutoScope::Song, tick, value);
                if let Some(lane) = self.project.automation_lane_mut(target, AutoScope::Song)
                    && let Some(p) = lane.points.iter_mut().find(|p| p.tick == tick)
                {
                    p.curve = curve;
                }
                self.drag = Some(super::app::Drag { v0: tick as f32, ..d });
                self.commit(tr("Move an automation point"));
            } else if over && input.right_down && let Some(i) = hover_point {
                self.project.remove_automation_point(target, AutoScope::Song, points[i].tick);
                self.commit(tr("Erase an automation point"));
            }

            self.draw_pass(c, target, x_of, &y_of, area);
            for (i, p) in points.iter().enumerate() {
                let (px, py) = (x_of(p.tick as f32), y_of(p.value));
                if px < area.x - 8.0 || px > area.right() + 8.0 {
                    continue;
                }
                let hot = hover_point == Some(i) && over;
                c.fill_circle(px, py, if hot { 5.5 } else { 4.0 }, t.bg_deep);
                c.stroke_circle(px, py, if hot { 5.5 } else { 4.0 }, 1.6, ink);
            }
            if over && let Some(i) = hover_point {
                let text = info.format(points[i].value);
                let (px, py) = (x_of(points[i].tick as f32), y_of(points[i].value));
                self.text(c, self.f.mono, 10.0, px + 8.0, py - 10.0, &text, t.text);
            }
        }
    }

    /// `PONZIK_CURVES=attack,release,cutoff,bend,song,stack,muted,list,other,param:<name>,show:<n>`:
    /// curves drawn in the current pattern on the piano roll's channel (its
    /// parameters by name), a song curve, the second stacked under the first,
    /// the last one muted, the automation list open, a curve on another
    /// channel; `show:n` jumps to the n-th curve of the list.
    pub fn screenshot_curves(&mut self, spec: &str) {
        let Some(ch) = self.piano_channel() else { return };
        let pat = self.project.current_pattern;
        let scope = AutoScope::Pattern(pat);
        let len = self.project.patterns[pat].len.max(1);
        let kind = self.project.channels[ch].instrument.kind.clone();
        let params = params_of(&kind, true);
        let mut drawn: Vec<PianoLane> = Vec::new();
        let shape = |app: &mut App, target: AutoTarget, info: &ParamInfo, k: usize| {
            let mut tick = 0;
            while tick <= len {
                let x = tick as f32 / len as f32;
                let n = match k % 3 {
                    0 => 0.15 + 0.7 * (x * std::f32::consts::TAU).sin().abs(),
                    1 => 0.2 + 0.6 * x,
                    _ => 0.85 - 0.6 * x * x,
                };
                app.project.add_automation_point(target, scope, tick, info.from_norm(n));
                tick += 24;
            }
        };
        for (k, part) in spec.split(',').map(str::trim).enumerate() {
            let name = match part {
                "attack" => Some("Attack"),
                "release" => Some("Release"),
                "cutoff" => Some("Cutoff"),
                p => p.strip_prefix("param:"),
            };
            if let Some(name) = name {
                if let Some(info) = params.iter().find(|p| p.name.eq_ignore_ascii_case(name)) {
                    shape(self, AutoTarget::InstrumentParam { channel: ch, id: info.id }, info, k);
                    drawn.push(PianoLane::Param(info.id));
                }
                continue;
            }
            match part {
                "bend" => {
                    let info = ParamInfo::new(0, "", -1.0, 1.0, 0.0, Unit::None);
                    shape(self, AutoTarget::PitchBend(ch), &info, k);
                    drawn.push(PianoLane::PitchBend);
                }
                "song" => {
                    let end = self.project.song_len().max(1);
                    let target = AutoTarget::ChannelGain(ch);
                    self.project.add_automation_point(target, AutoScope::Song, 0, 0.2);
                    self.project.add_automation_point(target, AutoScope::Song, end / 2, 1.0);
                    self.project.add_automation_point(target, AutoScope::Song, end, 0.5);
                }
                "other" => {
                    let other = (ch + 1) % self.project.channels.len().max(1);
                    if other != ch {
                        let info = ParamInfo::new(0, "", 0.0, 1.0, 0.0, Unit::Percent);
                        shape(self, AutoTarget::Cc { channel: other, cc: 1 }, &info, k);
                    }
                }
                "stack" => {
                    if let Some(&second) = drawn.get(1) {
                        self.piano.stack = vec![second];
                    }
                }
                "muted" => {
                    if let Some(a) = self.project.automation.last_mut() {
                        a.muted = true;
                    }
                }
                "list" => self.open_popup(super::app::Popup::AutomationList, Rect::default()),
                p => {
                    if let Some(n) = p.strip_prefix("show:").and_then(|n| n.parse::<usize>().ok())
                        && let Some(row) = self.curve_rows().get(n).cloned()
                    {
                        self.show_curve(row.target, row.scope);
                    }
                }
            }
        }
        if let Some(&first) = drawn.first() {
            self.piano.lane = first;
        }
    }
}
