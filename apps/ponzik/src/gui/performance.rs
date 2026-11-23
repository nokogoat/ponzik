//! Performance: the smart disable of idle effects, the eco mode of the
//! interface, the quality of exports, and the Settings tab that shows the
//! processor load and where it goes. See `docs/PERFORMANCE.md`.

use std::time::Duration;

use ponzik_i18n::{tr, tr_fmt};
use ponzik_project::json::Json;
use ponzik_ui::{Canvas, Rect};

use super::Input;
use super::app::App;
use super::settings::Glyph;
use super::widgets::{Icon, draw_icon, wid};

/// Eco mode: the interface counts as idle after this long without input.
pub const ECO_AFTER: Duration = Duration::from_secs(3);
/// Eco mode, idle: images per second while playing (playhead, meters).
pub const ECO_FRAME: Duration = Duration::from_micros(66_667);
/// Points of the load history (one every [`HISTORY_STEP`] seconds).
const HISTORY: usize = 96;
const HISTORY_STEP: f32 = 0.25;

/// Performance settings, saved in `settings.json`.
#[derive(Clone, Debug, PartialEq)]
pub struct Perf {
    /// Effects fed silence that give silence back stop being processed.
    pub smart_disable: bool,
    /// Idle interface: fewer images per second, no ambient animation.
    pub eco: bool,
    /// Exports run every effect on every block (exact); off, they sleep as live.
    pub exact_export: bool,
    /// Processor load, one point every [`HISTORY_STEP`] seconds (ring).
    history: [f32; HISTORY],
    at: usize,
    next: f32,
}

impl Default for Perf {
    fn default() -> Self {
        Self { smart_disable: true, eco: false, exact_export: true, history: [0.0; HISTORY], at: 0, next: 0.0 }
    }
}

impl Perf {
    pub fn load(&mut self, j: &Json) {
        let flag = |key: &str, default: bool| j.get(key).and_then(Json::as_bool).unwrap_or(default);
        self.smart_disable = flag("smart_disable", true);
        self.eco = flag("eco_mode", false);
        self.exact_export = flag("exact_export", true);
    }

    pub fn save(&self, fields: &mut Vec<(String, Json)>) {
        fields.push(("smart_disable".into(), self.smart_disable.into()));
        fields.push(("eco_mode".into(), self.eco.into()));
        fields.push(("exact_export".into(), self.exact_export.into()));
    }

    /// Records the load at `now` (seconds), one point per step.
    pub fn record(&mut self, now: f32, load: f32) {
        if now < self.next {
            return;
        }
        self.next = now + HISTORY_STEP;
        self.history[self.at] = load;
        self.at = (self.at + 1) % HISTORY;
    }

    /// The history from the oldest point to the newest.
    fn points(&self) -> impl Iterator<Item = f32> + '_ {
        (0..HISTORY).map(move |k| self.history[(self.at + k) % HISTORY])
    }
}

/// One line of the "heaviest" list.
struct Cost {
    icon: Icon,
    name: String,
    load: f32,
    asleep: bool,
}

impl App {
    /// Sends the performance settings where they act: the engine (smart
    /// disable) and the offline renders (export quality).
    pub fn apply_perf(&mut self) {
        self.send(ponzik_engine::Command::SetSmartDisable(self.prefs.perf.smart_disable));
        ponzik_project::set_fast_render(!self.prefs.perf.exact_export);
    }

    /// Instruments and effects by load, heaviest first; sleeping effects last.
    fn heaviest(&self, max: usize) -> (Vec<Cost>, usize) {
        let mut costs = Vec::new();
        for (ch, def) in self.project.channels.iter().enumerate() {
            let load = self.engine.channel_cpu(ch);
            if load >= 0.000_5 {
                costs.push(Cost { icon: Icon::Instrument, name: def.name.clone(), load, asleep: false });
            }
        }
        let mut asleep = 0;
        for (t, track) in self.project.tracks.iter().enumerate() {
            for (slot, fx) in track.inserts.iter().enumerate() {
                let Some(fx) = fx else { continue };
                let sleeping = self.engine.insert_asleep(t, slot);
                asleep += usize::from(sleeping);
                let load = self.engine.insert_cpu(t, slot);
                if load >= 0.000_5 || sleeping {
                    let icon = if fx.kind.contains(':') { Icon::Plugin } else { Icon::Effect };
                    let name = format!("{} · {}", track.name, self.device_name(super::devices::Device::Effect { track: t, slot }));
                    costs.push(Cost { icon, name, load, asleep: sleeping });
                }
            }
        }
        costs.sort_by(|a, b| a.asleep.cmp(&b.asleep).then(b.load.total_cmp(&a.load)));
        costs.truncate(max);
        (costs, asleep)
    }

    /// Settings › Performance: the load, its history and where it goes on the
    /// left; the switches that save it and the audio buffer on the right.
    pub(super) fn performance_tab(&mut self, c: &mut Canvas, input: &Input, body: Rect, hint: &mut Option<String>) {
        let t = self.theme;
        let left = Rect::new(body.x, body.y, (body.w * 0.55).round() - 12.0, body.h);
        let right = Rect::new(left.right() + 24.0, body.y, body.right() - left.right() - 24.0, body.h);

        // The load: a big number, its gauge and history.
        self.section(c, left.x, left.y, tr("Processor load"));
        let card = Rect::new(left.x, left.y + 20.0, left.w, 120.0);
        c.fill_rrect(card, 12.0, t.well);
        c.stroke_rrect(card, 12.0, 1.0, t.hairline);
        let load = self.load.max(0.0);
        let col = if load > 0.85 { t.negative } else if load > 0.6 { t.warning } else { t.accent };
        draw_icon(c, Icon::Cpu, card.x + 26.0, card.y + 30.0, 20.0, col);
        self.text(c, self.f.mono_med, 28.0, card.x + 46.0, card.y + 30.0, &tr_fmt!("{:.0}%", load * 100.0), t.text);
        let status = self.audio_status();
        let lines: Vec<&str> = status.split(['\n', '·']).map(str::trim).filter(|l| !l.is_empty()).collect();
        let gauge = Rect::new(card.x + 16.0, card.y + 50.0, card.w * 0.42, 8.0);
        c.fill_rrect(gauge, 4.0, t.meter_bg);
        if load > 0.002 {
            c.fill_rrect(Rect::new(gauge.x, gauge.y, (gauge.w * load.min(1.0)).max(8.0), gauge.h), 4.0, col);
        }
        for (k, line) in lines.iter().take(3).enumerate() {
            let col = if k == 0 { t.text2 } else { t.muted };
            self.text_fit(c, self.f.sans, 11.0, card.x + 16.0, card.y + 74.0 + k as f32 * 16.0, line, card.w * 0.5 - 20.0, col);
        }
        // History: the last 24 seconds, newest on the right.
        let graph = Rect::new(card.x + card.w * 0.5 + 8.0, card.y + 14.0, card.w * 0.5 - 24.0, card.h - 28.0);
        c.fill_rrect(graph, 8.0, t.bg_deep);
        for k in 1..4 {
            let y = graph.y + graph.h * k as f32 / 4.0;
            c.fill_rect(Rect::new(graph.x + 6.0, y, graph.w - 12.0, 1.0), t.hairline);
        }
        let step = (graph.w - 12.0) / (HISTORY - 1) as f32;
        let pts: Vec<(f32, f32)> = self
            .prefs
            .perf
            .points()
            .enumerate()
            .map(|(k, v)| (graph.x + 6.0 + k as f32 * step, graph.bottom() - 6.0 - (graph.h - 12.0) * v.clamp(0.0, 1.0)))
            .collect();
        c.stroke_polyline(&pts, 1.6, col);
        self.tip_zone(input, graph, tr("Processor load of the audio engine over the last 24 seconds"), "");

        // Where it goes.
        let y = card.bottom() + 18.0;
        let (costs, asleep) = self.heaviest(7);
        self.section(c, left.x, y, tr("Heaviest right now"));
        if asleep > 0 {
            let label = ponzik_i18n::tr_fmt_n!("{} effect asleep", "{} effects asleep", asleep, asleep);
            let w = self.fonts.measure(self.f.sans, 11.0, &label);
            draw_icon(c, Icon::Moon, left.right() - w - 14.0, y + 8.0, 12.0, t.muted);
            self.text_right(c, self.f.sans, 11.0, left.right(), y + 8.0, &label, t.muted);
        }
        let row_h = 30.0;
        if costs.is_empty() {
            let r = Rect::new(left.x, y + 20.0, left.w, row_h * 2.0);
            c.fill_rrect(r, 10.0, t.well.alpha(0.6));
            self.text_center(c, self.f.sans, 12.0, r.x + r.w * 0.5, r.y + r.h * 0.5, tr("Nothing is working hard: play the song to see the costs"), t.muted);
        }
        let top = costs.iter().map(|c| c.load).fold(0.01f32, f32::max);
        for (k, cost) in costs.iter().enumerate() {
            let r = Rect::new(left.x, y + 20.0 + k as f32 * (row_h + 4.0), left.w, row_h);
            if r.bottom() > body.bottom() {
                break;
            }
            let id = wid("perf-row", k, 0);
            let hov = self.hovered(input, id, r);
            let hv = self.hover_anim(id, hov);
            c.fill_rrect(r, 8.0, t.raised.mix(t.raised_hi, hv * 0.5).alpha(if cost.asleep { 0.6 } else { 1.0 }));
            let ink = if cost.asleep { t.muted } else { t.text2 };
            draw_icon(c, if cost.asleep { Icon::Moon } else { cost.icon }, r.x + 16.0, r.y + r.h * 0.5, 13.0, ink);
            let name_w = r.w * 0.5;
            self.text_fit(c, self.f.sans_med, 12.0, r.x + 32.0, r.y + r.h * 0.5, &cost.name, name_w - 36.0, if cost.asleep { t.muted } else { t.text });
            let bar = Rect::new(r.x + name_w, r.y + r.h * 0.5 - 3.0, r.w - name_w - 64.0, 6.0);
            c.fill_rrect(bar, 3.0, t.meter_bg);
            if !cost.asleep {
                c.fill_rrect(Rect::new(bar.x, bar.y, (bar.w * cost.load / top).max(6.0), bar.h), 3.0, t.meter);
            }
            let value = if cost.asleep { tr("asleep").to_string() } else { tr_fmt!("{:.1}%", cost.load * 100.0) };
            self.text_right(c, self.f.mono, 11.0, r.right() - 10.0, r.y + r.h * 0.5, &value, ink);
            if hov {
                *hint = Some(if cost.asleep {
                    tr_fmt!("{}: asleep, silence in and out; it wakes with the next sound", cost.name.as_str())
                } else {
                    tr_fmt!("{}: {:.1}% of one processor core", cost.name.as_str(), cost.load * 100.0)
                });
            }
        }

        // The switches.
        self.section(c, right.x, right.y, tr("Save processor and battery"));
        let gap = 8.0;
        let tile_w = ((right.w - 2.0 * gap) / 3.0).floor();
        let perf = self.prefs.perf.clone();
        let tiles = [
            (Icon::Moon, tr("Smart disable"), tr("Effects fed silence stop working until sound comes back, like FL Studio"), perf.smart_disable),
            (Icon::Leaf, tr("Eco mode"), tr("After 3 s without touching anything, the interface draws fewer images per second"), perf.eco),
            (Icon::Export, tr("Exact export"), tr("Exports run every effect all the time; off, they skip silent effects as live and finish sooner"), perf.exact_export),
        ];
        let mut toggled = None;
        for (k, (icon, label, desc, on)) in tiles.into_iter().enumerate() {
            let r = Rect::new(right.x + k as f32 * (tile_w + gap), right.y + 20.0, tile_w, 92.0);
            let id = wid("perf-tile", k, 0);
            if self.tile(c, input, id, r, Glyph::Icon(icon), label, on, "") {
                toggled = Some(k);
            }
            if self.hovered(input, id, r) {
                *hint = Some(format!("{label} · {desc}"));
            }
        }
        if let Some(k) = toggled {
            let p = &mut self.prefs.perf;
            match k {
                0 => p.smart_disable = !p.smart_disable,
                1 => p.eco = !p.eco,
                _ => p.exact_export = !p.exact_export,
            }
            self.apply_perf();
            self.save_settings();
        }
        let y = right.y + 20.0 + 92.0 + 22.0;
        self.audio_buffer_choice(c, input, right.x, y, right.w, hint);
        let latency = ponzik_audio::target_latency_ms();
        let note = tr_fmt!("{} ms between a key and its sound, plus the effects' own delay", latency);
        self.text_fit(c, self.f.sans, 11.0, right.x + 2.0, y + 20.0 + 2.0 * 36.0 + 14.0, &note, right.w - 4.0, t.muted);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perf_settings_round_trip_and_default() {
        let p = Perf::default();
        assert!(p.smart_disable && !p.eco && p.exact_export);
        let changed = Perf { smart_disable: false, eco: true, exact_export: false, ..Perf::default() };
        let mut fields = Vec::new();
        changed.save(&mut fields);
        let mut back = Perf::default();
        back.load(&Json::Obj(fields));
        assert_eq!(back, changed);
        // Missing or broken: the defaults.
        let mut broken = Perf::default();
        broken.load(&Json::Obj(vec![("eco_mode".into(), Json::Str("yes".into()))]));
        assert_eq!(broken, Perf::default());
    }

    /// Untouched and stopped, every view settles and stops asking for images:
    /// the window then sleeps until an event (idle processor near 0).
    #[test]
    fn idle_interface_stops_asking_for_images() {
        use super::super::app::View;
        for view in [View::Playlist, View::Piano, View::Mixer, View::Rack, View::Session] {
            let project = ponzik_project::demo_project();
            let (mut engine, handle) = project.build_engine(48_000.0, &|_| None);
            let mut app = App::new(handle, project, 48_000);
            app.set_view(view);
            let mut canvas = Canvas::new(1280, 800);
            let input = Input::default();
            let mut audio = vec![0.0f32; 800 * 2];
            let mut settled = None;
            for k in 0..600 {
                engine.process(&mut audio, 2);
                app.frame(&mut canvas, &input, 1.0 / 60.0);
                if !app.needs_redraw(false) {
                    settled = Some(k);
                    break;
                }
            }
            assert!(settled.is_some(), "{view:?} keeps asking for images while idle");
            // Playing: images; stopped again, it settles again (eco or not).
            app.toggle_play();
            for _ in 0..30 {
                engine.process(&mut audio, 2);
                app.frame(&mut canvas, &input, 1.0 / 60.0);
            }
            assert!(app.needs_redraw(true), "{view:?}: playing draws");
            app.toggle_play();
            let mut eco_settled = false;
            for _ in 0..600 {
                engine.process(&mut audio, 2);
                app.frame(&mut canvas, &input, 1.0 / 60.0);
                if !app.needs_redraw(true) {
                    eco_settled = true;
                    break;
                }
            }
            assert!(eco_settled, "{view:?} keeps drawing after stop in eco mode");
        }
    }

    #[test]
    fn load_history_keeps_one_point_per_step() {
        let mut p = Perf::default();
        for k in 0..40 {
            p.record(k as f32 * 0.05, k as f32 / 40.0);
        }
        // 2 s at one point every 0.25 s.
        assert_eq!(p.points().filter(|&v| v > 0.0).count(), 7);
        assert_eq!(p.points().last(), Some(35.0 / 40.0));
    }
}
