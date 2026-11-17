//! Couloirs d'automation dans la playlist : une courbe par paramètre automatisé.

use ponzik_engine::{AutoScope, AutoTarget, ParamInfo, Unit};
use ponzik_i18n::{tr, tr_fmt, tr_noop};
use ponzik_ui::{Canvas, Path, Rect};

use super::Input;
use super::app::App;
use super::devices::params_of;

use super::widgets::wid;

pub const AUTO_LANE_H: f32 = 56.0;
const POINT_HIT: f32 = 8.0;

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
        }
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

    pub fn is_automated(&self, target: AutoTarget) -> bool {
        self.project.automation.iter().any(|a| a.target == target && !a.points.is_empty())
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
            c.fill_path(&path, t.select.alpha(0.12));
            c.stroke_polyline(&pts, 1.6, t.select);

            let hover_point = points.iter().position(|p| {
                (input.x - x_of(p.tick as f32)).abs() < POINT_HIT && (input.y - y_of(p.value)).abs() < POINT_HIT
            });
            let over = input.inside && row.contains(input.x, input.y) && (self.drag.is_none() || self.is_dragging(lane_id));

            // Nom du couloir, en pastille.
            let tw = self.fonts.measure(self.f.sans_med, 11.0, &label) + 16.0;
            let chip = Rect::new(area.x + 6.0, row.y + 4.0, tw, 18.0);
            c.fill_rrect(chip, 6.0, t.bg_deep.alpha(0.85));
            self.text(c, self.f.sans_med, 11.0, chip.x + 8.0, chip.y + 9.0, &label, t.select);
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
                c.stroke_circle(px, py, if hot { 5.5 } else { 4.0 }, 1.6, t.select);
            }
            if over && let Some(i) = hover_point {
                let text = info.format(points[i].value);
                let (px, py) = (x_of(points[i].tick as f32), y_of(points[i].value));
                self.text(c, self.f.mono, 10.0, px + 8.0, py - 10.0, &text, t.text);
            }
        }
    }
}
