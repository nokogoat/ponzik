//! Lanes under the piano roll's velocities: per-note chance, condition,
//! repeats, pan and fine pitch, and the pattern's controller lanes (pitch
//! bend, any CC, the instrument's parameters) drawn with a pencil or a line.

use ponzik_engine::{AutoScope, AutoTarget, ParamInfo};
use ponzik_i18n::{tr, tr_fmt};
use ponzik_ui::{Canvas, Path, Rect};
use ponzik_window::Cursor;

use super::Input;
use super::app::{Action, App, MenuItem, Popup};
use super::automation::cc_name;
use super::devices::params_of;
use super::widgets::{Icon, draw_icon, wid};

/// What the lane under the velocities shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PianoLane {
    #[default]
    Chance,
    Condition,
    Repeats,
    /// Pan of each note.
    Pan,
    /// Fine pitch of each note, in cents.
    Fine,
    /// The pattern's pitch bend lane of the channel.
    PitchBend,
    /// The pattern's lane of a control change.
    Cc(u8),
    /// The pattern's lane of an instrument parameter.
    Param(u32),
}

/// Control changes offered first in the lane menu.
const COMMON_CCS: [u8; 6] = [1, 11, 74, 71, 2, 64];

impl PianoLane {
    /// Lanes of a note's own settings.
    pub const NOTE: [PianoLane; 5] = [Self::Chance, Self::Condition, Self::Repeats, Self::Pan, Self::Fine];

    /// Note lane `i` (0 chance … 4 fine pitch).
    pub fn note(i: usize) -> Self {
        Self::NOTE[i.min(Self::NOTE.len() - 1)]
    }

    /// Index of a note lane (0 to 4), `None` for a controller lane.
    pub fn note_index(self) -> Option<usize> {
        Self::NOTE.iter().position(|&l| l == self)
    }

    /// The automation target of a controller lane on channel `ch`.
    pub fn target(self, ch: usize) -> Option<AutoTarget> {
        match self {
            Self::PitchBend => Some(AutoTarget::PitchBend(ch)),
            Self::Cc(cc) => Some(AutoTarget::Cc { channel: ch, cc }),
            Self::Param(id) => Some(AutoTarget::InstrumentParam { channel: ch, id }),
            _ => None,
        }
    }

    fn name(self) -> String {
        match self {
            Self::Chance => tr("Chance").into(),
            Self::Condition => tr("Condition").into(),
            Self::Repeats => tr("Repeats").into(),
            Self::Pan => tr("Pan").into(),
            Self::Fine => tr("Fine pitch").into(),
            Self::PitchBend => tr("Pitch bend").into(),
            Self::Cc(cc) => cc_name(cc),
            Self::Param(_) => String::new(),
        }
    }

    fn icon(self) -> Icon {
        match self {
            Self::Chance => Icon::Dice,
            Self::Condition => Icon::Columns,
            Self::Repeats => Icon::Repeat,
            Self::Pan => Icon::Speaker,
            Self::Fine => Icon::Scale,
            Self::PitchBend => Icon::Glide,
            Self::Cc(_) => Icon::Midi,
            Self::Param(_) => Icon::Automation,
        }
    }
}

impl App {
    /// Parameter sheet of a lane's instrument parameter, if any.
    fn lane_param(&self, ch: usize, id: u32) -> Option<&'static ParamInfo> {
        let kind = &self.project.channels.get(ch)?.instrument.kind;
        params_of(kind, true).iter().find(|p| p.id == id)
    }

    /// Short label of the current lane, for its selector.
    fn lane_short(&self, ch: usize) -> String {
        match self.piano.lane {
            PianoLane::Chance => "%".into(),
            PianoLane::Condition => "N:M".into(),
            PianoLane::Repeats => "×".into(),
            PianoLane::Pan => tr("Pan").into(),
            PianoLane::Fine => tr("Fine").into(),
            PianoLane::PitchBend => tr("Bend").into(),
            PianoLane::Cc(1) => tr("Mod").into(),
            PianoLane::Cc(cc) => format!("CC{cc}"),
            PianoLane::Param(id) => self.lane_param(ch, id).map_or_else(|| "?".into(), |p| tr(p.name).to_string()),
        }
    }

    /// True when the pattern has a controller lane drawn for `target`.
    fn lane_drawn(&self, target: AutoTarget) -> bool {
        let scope = AutoScope::Pattern(self.project.current_pattern);
        self.project.automation_lane(target, scope).is_some_and(|l| !l.points.is_empty())
    }

    /// Menu of the lanes: the notes' own settings, then the channel's controllers.
    pub(super) fn lane_menu(&self) -> (String, Vec<MenuItem>) {
        let Some(ch) = self.piano_channel() else { return (String::new(), Vec::new()) };
        let cur = self.piano.lane;
        let drawn = |lane: PianoLane| lane.target(ch).is_some_and(|t| self.lane_drawn(t));
        let item = |lane: PianoLane, tip: &str| {
            let it = MenuItem::new(lane.icon(), &lane.name(), Action::PianoLane(lane)).on(cur == lane).tip(tip);
            if drawn(lane) { it.detail(tr("drawn").into()) } else { it }
        };
        let mut v = vec![
            item(PianoLane::Chance, tr("How often each note plays")),
            item(PianoLane::Condition, tr("On which passes of the pattern a note plays")),
            item(PianoLane::Repeats, tr("Ratchets: the note fires several times within its length")),
            item(PianoLane::Pan, tr("Pan of each note")),
            item(PianoLane::Fine, tr("Fine pitch of each note, in cents")),
        ];
        v.push(item(PianoLane::PitchBend, tr("Pitch bend of the channel, drawn in the pattern")).sep());
        let mut ccs: Vec<u8> = COMMON_CCS.to_vec();
        // Other CCs already drawn in this pattern, and the current one.
        for a in &self.project.automation {
            if let AutoTarget::Cc { channel, cc } = a.target
                && channel == ch
                && a.scope == AutoScope::Pattern(self.project.current_pattern)
                && !ccs.contains(&cc)
            {
                ccs.push(cc);
            }
        }
        if let PianoLane::Cc(cc) = cur
            && !ccs.contains(&cc)
        {
            ccs.push(cc);
        }
        for cc in ccs {
            v.push(item(PianoLane::Cc(cc), &tr_fmt!("Control change {} · wheel on the lane name: another number", cc)));
        }
        let kind = &self.project.channels[ch].instrument.kind;
        for (i, p) in params_of(kind, true).iter().filter(|p| !p.choices.is_empty() || p.max > p.min).enumerate() {
            let lane = PianoLane::Param(p.id);
            let it = MenuItem::new(Icon::Automation, tr(p.name), Action::PianoLane(lane)).on(cur == lane).tip(tr("A parameter of the instrument, drawn in the pattern"));
            let it = if drawn(lane) { it.detail(tr("drawn").into()) } else if p.group.is_empty() { it } else { it.detail(tr(p.group).into()) };
            v.push(if i == 0 { it.sep() } else { it });
        }
        (tr("Lane").into(), v)
    }

    /// Lane selector (opens the menu, wheel: next lane or CC number) and the
    /// pencil and line tools of a controller lane.
    pub(super) fn lane_selector(&mut self, c: &mut Canvas, input: &Input, ch: usize, area: Rect) {
        let t = self.theme;
        let chip = Rect::new(area.x, area.y, area.w, 24.0);
        let id = wid("pr-lane-pick", 0, 0);
        let hov = self.hovered(input, id, chip);
        let hv = self.hover_anim(id, hov);
        let open = matches!(self.popup.as_ref().map(|p| &p.kind), Some(Popup::PianoLanes));
        c.fill_rrect(chip, 7.0, t.raised.mix(t.raised_hi, hv.max(open as u8 as f32)).mix(t.accent, 0.15));
        let label = self.lane_short(ch);
        let label = self.fit(self.f.mono_med, 11.0, &label, chip.w - 18.0);
        self.text(c, self.f.mono_med, 11.0, chip.x + 6.0, chip.y + 12.0, &label, t.text.mix(t.accent, 0.3 * hv));
        draw_icon(c, Icon::ChevronDown, chip.right() - 8.0, chip.y + 12.0, 8.0, t.muted);
        // Curves drawn in other lanes keep playing while hidden: say so.
        let shown = self.piano.lane.target(ch);
        let scope = AutoScope::Pattern(self.project.current_pattern);
        let hidden = self.project.automation.iter().filter(|a| a.scope == scope && !a.points.is_empty() && Some(a.target) != shown).count();
        if hidden > 0 {
            let (bx, by) = (chip.right() - 2.0, chip.y + 2.0);
            c.fill_circle(bx, by, 7.0, t.select);
            let n = hidden.min(9).to_string();
            let tw = self.fonts.measure(self.f.mono_med, 10.0, &n);
            self.text(c, self.f.mono_med, 10.0, bx - tw * 0.5, by, &n, t.on_accent);
        }
        if hov {
            self.set_cursor(Cursor::Hand);
            if hidden > 0 {
                self.set_tip(&tr_fmt!("Lane\n{} other curves are drawn in this pattern and keep playing: open the menu to see them", hidden), "");
            } else {
                self.set_tip(tr("Lane\nClick: chance, condition, repeats, pan, fine pitch, pitch bend, CCs, parameters · wheel: next"), "");
            }
            if input.pressed {
                self.open_popup(Popup::PianoLanes, chip);
            }
            if input.wheel != 0.0 {
                let up = input.wheel > 0.0;
                self.piano.lane = match self.piano.lane {
                    PianoLane::Cc(cc) => PianoLane::Cc(if up { cc.saturating_add(1).min(127) } else { cc.saturating_sub(1) }),
                    lane => {
                        let order = [PianoLane::Chance, PianoLane::Condition, PianoLane::Repeats, PianoLane::Pan, PianoLane::Fine, PianoLane::PitchBend];
                        let i = order.iter().position(|&l| l == lane).unwrap_or(0) as i32 + if up { -1 } else { 1 };
                        order[i.rem_euclid(order.len() as i32) as usize]
                    }
                };
            }
        }
        if self.piano.lane.target(ch).is_none() {
            return;
        }
        let w = (area.w - 4.0) / 2.0;
        let tools = [(Icon::Pencil, false, tr("Pencil\nDraw the curve freehand · Shift: a line")), (Icon::Line, true, tr("Line\nDrag a straight line"))];
        for (i, (icon, line, tip)) in tools.into_iter().enumerate() {
            let r = Rect::new(area.x + i as f32 * (w + 4.0), chip.bottom() + 6.0, w, 24.0);
            if self.icon_button(c, input, wid("pr-lane-tool", i, 0), r, icon, self.piano.line_tool == line, (tip, "")) {
                self.piano.line_tool = line;
            }
        }
    }

    /// A controller lane of the pattern: its curve, drawn with the pencil
    /// (or a line), right-click erases its points.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn event_lane(
        &mut self,
        c: &mut Canvas,
        input: &Input,
        target: AutoTarget,
        lane: Rect,
        x_of: &dyn Fn(f32) -> f32,
        tick_at: &dyn Fn(f32) -> f32,
    ) {
        let t = self.theme;
        c.fill_rrect(lane, 10.0, t.well);
        let Some((_, info)) = self.target_info(target) else { return };
        let pat = self.project.current_pattern;
        let scope = AutoScope::Pattern(pat);
        let pat_len = self.project.patterns[pat].len.max(1);
        let prev = c.push_clip(lane);
        let (top, h) = (lane.y + 6.0, lane.h - 12.0);
        let base = top + h;
        let y_of = |v: f32| base - h * info.to_norm(v);
        let value_at = |y: f32| {
            let v = info.from_norm(((base - y) / h).clamp(0.0, 1.0));
            if info.choices.is_empty() { v } else { v.round() }
        };
        let res = (self.piano.snap / 4).max(3);
        let snap = |tick: f32| ((tick / res as f32).round() * res as f32).clamp(0.0, pat_len as f32) as u32;
        let bipolar = info.min < 0.0 && info.max > 0.0;
        let rest_y = if bipolar { y_of(0.0) } else { base };
        if bipolar {
            c.fill_rect(Rect::new(lane.x, rest_y.round(), lane.w, 1.0), t.hairline.alpha(2.5));
        }
        // Past the pattern's end: dimmed.
        let end_x = x_of(pat_len as f32);
        if end_x < lane.right() {
            c.fill_rect(Rect::new(end_x, lane.y, lane.right() - end_x, lane.h), t.bg_deep.alpha(0.45));
        }

        let id = wid("pr-ev-lane", 0, 0);
        let hov = self.hovered(input, id, lane);
        if hov || self.is_dragging(id) {
            self.set_cursor(Cursor::Crosshair);
        }
        let line = self.piano.line_tool || input.mods.shift;
        let here = (snap(tick_at(input.x)), value_at(input.y));
        if hov && input.pressed {
            self.begin_drag(id, input, 0.0);
            self.piano.lane_anchor = Some(here);
            if !line {
                self.project.add_automation_point(target, scope, here.0, here.1);
                self.commit(tr("Draw a controller"));
            }
        }
        if self.is_dragging(id)
            && let Some((t0, v0)) = self.piano.lane_anchor
        {
            if line {
                c.stroke_polyline(&[(x_of(t0 as f32), y_of(v0)), (x_of(here.0 as f32), y_of(here.1))], 2.0, t.select);
                c.fill_circle(x_of(t0 as f32), y_of(v0), 3.5, t.select);
                if input.released {
                    let (a, b) = if t0 <= here.0 { ((t0, v0), here) } else { (here, (t0, v0)) };
                    self.project.erase_automation(target, scope, a.0, b.0);
                    self.project.add_automation_point(target, scope, a.0, a.1);
                    self.project.add_automation_point(target, scope, b.0, b.1);
                    self.commit(tr("Draw a controller"));
                }
            } else if here.0 != t0 {
                // Freehand: the stretch since the last point, one point per step.
                let (a, b) = if t0 < here.0 { ((t0, v0), here) } else { (here, (t0, v0)) };
                self.project.erase_automation(target, scope, a.0, b.0);
                let mut tick = a.0;
                while tick <= b.0 {
                    let k = (tick - a.0) as f32 / (b.0 - a.0).max(1) as f32;
                    let v = a.1 + (b.1 - a.1) * k;
                    self.project.add_automation_point(target, scope, tick, if info.choices.is_empty() { v } else { v.round() });
                    tick += res;
                }
                self.project.add_automation_point(target, scope, b.0, b.1);
                self.piano.lane_anchor = Some(here);
                self.commit(tr("Draw a controller"));
            } else if (here.1 - v0).abs() > f32::EPSILON {
                self.project.add_automation_point(target, scope, here.0, here.1);
                self.piano.lane_anchor = Some(here);
                self.commit(tr("Draw a controller"));
            }
            self.text(c, self.f.mono_med, 11.0, (input.x + 10.0).min(lane.right() - 60.0), lane.y + 12.0, &info.format(here.1), t.text);
        }
        if input.released && self.piano.lane_anchor.is_some() && !self.is_dragging(id) {
            self.piano.lane_anchor = None;
        }
        // Right button held: erase the points swept.
        if hov && input.right_down && self.drag.is_none() {
            let (a, b) = (tick_at(input.x - 6.0).max(0.0) as u32, tick_at(input.x + 6.0).max(0.0) as u32);
            if self.project.erase_automation(target, scope, a, b) {
                self.commit(tr("Erase a controller"));
            }
        }

        // The curve, filled from its resting value.
        let points = self.project.automation_lane(target, scope).map(|l| l.points.clone()).unwrap_or_default();
        if points.is_empty() {
            let hint = tr("Draw here · Shift: a line · right-click: erase");
            self.text_center(c, self.f.sans, 12.0, lane.x + lane.w * 0.5, lane.y + lane.h * 0.5, hint, t.muted.alpha(0.7));
        } else {
            let mut pts = Vec::new();
            let mut x = lane.x;
            while x <= lane.right().min(end_x) + 2.0 {
                let tick = tick_at(x).clamp(0.0, pat_len as f32) as f64;
                if let Some(v) = ponzik_engine::sequence::automation_value(&points, tick) {
                    pts.push((x, y_of(v)));
                }
                x += 2.0;
            }
            if let (Some(&(x0, _)), Some(&(x1, _))) = (pts.first(), pts.last()) {
                let mut fill = Path::new();
                let mut poly = pts.clone();
                poly.push((x1, rest_y));
                poly.push((x0, rest_y));
                fill.polygon(&poly);
                c.fill_path(&fill, t.accent.alpha(0.16));
                c.stroke_polyline(&pts, 1.6, t.accent);
            }
            let zoom = self.piano.zoom;
            let dense = points.windows(2).any(|w| (w[1].tick - w[0].tick) as f32 * zoom < 5.0);
            if !dense {
                for p in &points {
                    let x = x_of(p.tick as f32);
                    if x >= lane.x - 4.0 && x <= lane.right() + 4.0 {
                        c.fill_circle(x, y_of(p.value), 2.6, t.accent_hot);
                    }
                }
            }
        }
        c.set_clip(Some(prev));
        if hov && self.drag.is_none() {
            let tick = tick_at(input.x).clamp(0.0, pat_len as f32) as f64;
            let v = ponzik_engine::sequence::automation_value(&points, tick).unwrap_or(if bipolar { 0.0 } else { info.default });
            self.set_tip(&format!("{} · {}", self.lane_short_name(target), info.format(v)), "");
        }
    }

    fn lane_short_name(&self, target: AutoTarget) -> String {
        self.target_info(target).map(|(name, _)| name).unwrap_or_default()
    }
}

/// Labels of the per-note lanes' values.
pub fn pan_label(pan: f32) -> String {
    let p = (pan * 100.0).round() as i32;
    match p {
        0 => "C".into(),
        p if p < 0 => format!("L{}", -p),
        p => format!("R{p}"),
    }
}
