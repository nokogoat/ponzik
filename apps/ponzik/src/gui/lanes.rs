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

/// Height of the row of lane chips.
pub const CHIPS_H: f32 = 22.0;
const CHIP_GAP: f32 = 4.0;
/// Width of a chip's small curve, and the most its name takes.
const SPARK_W: f32 = 34.0;
const CURVE_NAME_MAX: f32 = 110.0;
/// A lane is stacked under the one shown only if each keeps this height.
const STACK_MIN_H: f32 = 30.0;
const MAX_STACKED: usize = 2;

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

    /// The lane showing a pattern curve of `target`, with its channel.
    pub fn of_target(target: AutoTarget) -> Option<(usize, Self)> {
        match target {
            AutoTarget::PitchBend(ch) => Some((ch, Self::PitchBend)),
            AutoTarget::Cc { channel, cc } => Some((channel, Self::Cc(cc))),
            AutoTarget::InstrumentParam { channel, id } => Some((channel, Self::Param(id))),
            _ => None,
        }
    }

    /// A number for the lane's widgets, different for every lane.
    fn key(self) -> usize {
        match self {
            Self::Chance => 0,
            Self::Condition => 1,
            Self::Repeats => 2,
            Self::Pan => 3,
            Self::Fine => 4,
            Self::PitchBend => 5,
            Self::Cc(cc) => 16 + cc as usize,
            Self::Param(id) => 1_000 + id as usize,
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
        if hov {
            self.set_cursor(Cursor::Hand);
            self.set_tip(tr("Lane\nClick: chance, condition, repeats, pan, fine pitch, pitch bend, CCs, parameters · wheel: next"), "");
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

    /// Controller lanes of channel `ch` drawn in the current pattern, in the
    /// project's order; the lane shown is added (empty) if it is a controller.
    pub(super) fn drawn_lanes(&self, ch: usize) -> Vec<PianoLane> {
        let scope = AutoScope::Pattern(self.project.current_pattern);
        let mut v: Vec<PianoLane> = self
            .project
            .automation
            .iter()
            .filter(|a| a.scope == scope && !a.points.is_empty())
            .filter_map(|a| PianoLane::of_target(a.target).filter(|&(c, _)| c == ch).map(|(_, l)| l))
            .collect();
        let cur = self.piano.lane;
        if cur.target(ch).is_some() && !v.contains(&cur) {
            v.push(cur);
        }
        v
    }

    /// Curves of the current pattern on other channels (they play too).
    fn other_channel_curves(&self, ch: usize) -> usize {
        let scope = AutoScope::Pattern(self.project.current_pattern);
        self.project.automation.iter().filter(|a| a.scope == scope && !a.points.is_empty() && a.target.channel() != Some(ch)).count()
    }

    /// The lane area under the velocities: its selector on the left, a row of
    /// chips on top (the notes' lanes, every curve drawn, +), then the lane
    /// shown and the lanes stacked under it when there is room. Returns the
    /// rectangle of the lane shown.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn lane_area(
        &mut self,
        c: &mut Canvas,
        input: &Input,
        ch: usize,
        tabs: Rect,
        lane: Rect,
        x_of: &dyn Fn(f32) -> f32,
        tick_at: &dyn Fn(f32) -> f32,
    ) -> Rect {
        self.lane_selector(c, input, ch, tabs);
        let strip = Rect::new(lane.x, lane.y, lane.w, CHIPS_H);
        self.lane_chips(c, input, ch, strip);
        let body = Rect::new(lane.x, strip.bottom() + 4.0, lane.w, (lane.h - CHIPS_H - 4.0).max(16.0));
        let cur = self.piano.lane;
        self.piano.stack.retain(|&l| l != cur && l.target(ch).is_some());
        let stacked: Vec<AutoTarget> = self.piano.stack.iter().filter_map(|l| l.target(ch)).collect();
        let fit = ((body.h + 4.0) / (STACK_MIN_H + 4.0)).floor() as usize;
        let extra = stacked.len().min(fit.saturating_sub(1)).min(MAX_STACKED);
        if extra == 0 {
            return body;
        }
        let each = ((body.h - 4.0 * extra as f32) / (extra + 1) as f32).floor();
        for (i, &target) in stacked[..extra].iter().enumerate() {
            let r = Rect::new(body.x, body.y + (i + 1) as f32 * (each + 4.0), body.w, each);
            self.event_lane(c, input, target, r, x_of, tick_at);
        }
        Rect::new(body.x, body.y, body.w, each)
    }

    /// Chips above the lane: the notes' own lanes, one per curve drawn in
    /// the pattern (name, small curve, × to delete), then + for any lane.
    fn lane_chips(&mut self, c: &mut Canvas, input: &Input, ch: usize, strip: Rect) {
        let t = self.theme;
        let cur = self.piano.lane;
        let scope = AutoScope::Pattern(self.project.current_pattern);
        let curves = self.drawn_lanes(ch);
        let others = self.other_channel_curves(ch);
        let buttons_w = 2.0 * (CHIPS_H + 4.0);
        let others_w = if others > 0 { 64.0 } else { 0.0 };
        // Curve chips first get their room; the notes' lanes shrink to icons when short of it.
        let curve_w = |app: &App, lane: PianoLane| -> f32 {
            let name = app.lane_label(ch, lane);
            (app.fonts.measure(app.f.sans_med, 11.0, &name).min(CURVE_NAME_MAX) + 12.0 + SPARK_W + 6.0 + 16.0).ceil()
        };
        let curves_total: f32 = curves.iter().map(|&l| curve_w(self, l) + CHIP_GAP).sum();
        let note_full: f32 = PianoLane::NOTE.iter().map(|&l| self.fonts.measure(self.f.sans_med, 11.0, &l.name()) + 34.0 + CHIP_GAP).sum();
        let room = strip.w - buttons_w - others_w - curves_total - 10.0;
        let icons_only = note_full > room;
        let mut x = strip.x;
        let prev = c.push_clip(Rect::new(strip.x, strip.y - 2.0, strip.w - buttons_w, strip.h + 4.0));
        for (i, lane) in PianoLane::NOTE.into_iter().enumerate() {
            let name = lane.name();
            let w = if icons_only { CHIPS_H + 4.0 } else { (self.fonts.measure(self.f.sans_med, 11.0, &name) + 34.0).ceil() };
            let r = Rect::new(x, strip.y, w, CHIPS_H);
            let id = wid("pr-chip-note", i, 0);
            let hov = self.hovered(input, id, r);
            let hv = self.hover_anim(id, hov);
            let on = cur == lane;
            c.fill_rrect(r, 7.0, if on { t.accent.alpha(0.22) } else { t.raised.alpha(0.55 + 0.45 * hv) });
            let col = if on { t.accent } else { t.text2.mix(t.text, hv) };
            draw_icon(c, lane.icon(), r.x + 13.0, r.y + CHIPS_H * 0.5, 13.0, col);
            if !icons_only {
                self.text(c, self.f.sans_med, 11.0, r.x + 25.0, r.y + CHIPS_H * 0.5, &name, col);
            }
            if hov {
                self.set_cursor(Cursor::Hand);
                self.set_tip(&tr_fmt!("{}\nA setting of each note", name), "");
                if input.pressed {
                    self.piano.lane = lane;
                }
            }
            x += w + CHIP_GAP;
        }
        if !curves.is_empty() {
            c.fill_rect(Rect::new(x + 2.0, strip.y + 4.0, 1.0, CHIPS_H - 8.0), t.hairline.alpha(2.0));
            x += 8.0;
        }
        for lane in curves {
            let Some(target) = lane.target(ch) else { continue };
            let w = curve_w(self, lane);
            let r = Rect::new(x, strip.y, w, CHIPS_H);
            x += w + CHIP_GAP;
            let key = lane.key();
            let id = wid("pr-chip-curve", key, 0);
            let close_r = Rect::new(r.right() - 18.0, r.y + 2.0, 16.0, CHIPS_H - 4.0);
            let hov = self.hovered(input, id, r);
            let hv = self.hover_anim(id, hov);
            let on = cur == lane;
            let stacked = self.piano.stack.contains(&lane);
            let lane_def = self.project.automation_lane(target, scope);
            let muted = lane_def.is_some_and(|l| l.muted);
            let points = lane_def.map(|l| l.points.clone()).unwrap_or_default();
            let flash = self.curve_flash(target, scope);
            c.fill_rrect(r, 7.0, if on { t.select.alpha(0.22) } else { t.raised.alpha(0.55 + 0.45 * hv) });
            if stacked || flash > 0.0 {
                c.stroke_rrect(r, 7.0, 1.2, t.select.alpha(if stacked { 0.7f32 } else { 0.0 }.max(flash)));
            }
            let col = if muted { t.muted } else if on { t.select } else { t.text2.mix(t.text, hv) };
            let name = self.lane_label(ch, lane);
            let name = self.fit(self.f.sans_med, 11.0, &name, CURVE_NAME_MAX);
            self.text(c, self.f.sans_med, 11.0, r.x + 8.0, r.y + CHIPS_H * 0.5, &name, col);
            let spark = Rect::new(r.right() - 18.0 - SPARK_W - 4.0, r.y + 4.0, SPARK_W, CHIPS_H - 8.0);
            if let Some((_, info)) = self.target_info(target) {
                if points.is_empty() {
                    c.fill_rect(Rect::new(spark.x, spark.bottom() - 1.0, spark.w, 1.0), t.muted.alpha(0.5));
                } else {
                    let now = self.live_tick(scope);
                    self.sparkline(c, spark, &points, &info, self.scope_len(scope), if muted { t.muted } else { t.select }, now);
                }
            }
            let over_close = hov && close_r.contains(input.x, input.y) && !points.is_empty();
            if !points.is_empty() {
                draw_icon(c, Icon::Close, close_r.x + 8.0, close_r.y + close_r.h * 0.5, 9.0, if over_close { t.negative } else { t.muted.mix(t.text2, hv) });
            }
            if hov {
                self.set_cursor(Cursor::Hand);
                if over_close {
                    self.set_tip(&tr_fmt!("Delete the curve\n{}", name), "");
                } else {
                    let state = if muted { tr("Muted: kept but not played") } else if points.is_empty() { tr("Empty: draw it in the lane") } else { tr("Plays with the pattern") };
                    self.set_tip(&tr_fmt!("{}\n{} · Shift+click: stack it under the lane shown · right-click: more", name, state), "");
                    self.tips.curve = Some(target);
                }
                if input.pressed {
                    if over_close {
                        self.delete_lane_curve(ch, lane);
                    } else if input.mods.shift && !on {
                        if stacked {
                            self.piano.stack.retain(|&l| l != lane);
                        } else {
                            self.piano.stack.push(lane);
                        }
                    } else {
                        self.piano.lane = lane;
                    }
                }
                if input.right_pressed && self.drag.is_none() {
                    let (_, info) = self.target_info(target).unwrap_or_else(|| (String::new(), ParamInfo::new(0, "", 0.0, 1.0, 0.0, ponzik_engine::Unit::None)));
                    let value = self.auto_value(target).unwrap_or(info.default);
                    self.open_popup(Popup::Param { target, value, default: info.default }, Rect::new(r.x, r.bottom(), 0.0, 0.0));
                }
            }
        }
        c.set_clip(Some(prev));
        // Curves of the other channels: to the automation list.
        let mut bx = strip.right() - buttons_w;
        if others > 0 {
            let r = Rect::new(bx - others_w, strip.y, others_w - CHIP_GAP, CHIPS_H);
            let id = wid("pr-chip-others", 0, 0);
            let hov = self.hovered(input, id, r);
            let hv = self.hover_anim(id, hov);
            c.stroke_rrect(r, 7.0, 1.0, t.hairline.alpha(1.5 + hv));
            draw_icon(c, Icon::Automation, r.x + 12.0, r.y + CHIPS_H * 0.5, 12.0, t.muted.mix(t.text, hv));
            self.text(c, self.f.mono_med, 11.0, r.x + 24.0, r.y + CHIPS_H * 0.5, &format!("+{others}"), t.text2.mix(t.text, hv));
            if hov {
                self.set_cursor(Cursor::Hand);
                self.set_tip(&ponzik_i18n::tr_fmt_n!("{} curve on other channels\nThis pattern plays it too · click: the automation list", "{} curves on other channels\nThis pattern plays them too · click: the automation list", others, others), "");
                if input.pressed {
                    self.open_popup(Popup::AutomationList, Rect::default());
                }
            }
        }
        let add = Rect::new(bx + 4.0, strip.y, CHIPS_H, CHIPS_H);
        let open = matches!(self.popup.as_ref().map(|p| &p.kind), Some(Popup::PianoLanes));
        if self.icon_button(c, input, wid("pr-chip-add", 0, 0), add, Icon::Plus, open, (tr("Add a lane\nPitch bend, a CC or any parameter of the instrument"), "")) {
            self.open_popup(Popup::PianoLanes, add);
        }
        bx = add.right() + 4.0;
        let list = Rect::new(bx, strip.y, CHIPS_H, CHIPS_H);
        if self.icon_button(c, input, wid("pr-chip-list", 0, 0), list, Icon::Automation, false, (tr("Automation list\nEvery curve of the project"), "")) {
            self.open_popup(Popup::AutomationList, Rect::default());
        }
    }

    /// The × of a curve's chip: its curve in the current pattern goes (the
    /// lane stays shown, empty, to draw again).
    pub(super) fn delete_lane_curve(&mut self, ch: usize, lane: PianoLane) {
        let Some(target) = lane.target(ch) else { return };
        self.piano.stack.retain(|&l| l != lane);
        self.delete_curve(target, Some(AutoScope::Pattern(self.project.current_pattern)));
    }

    /// Name of a lane for its chip: the parameter's own name for an instrument's.
    fn lane_label(&self, ch: usize, lane: PianoLane) -> String {
        match lane {
            PianoLane::Param(id) => self.lane_param(ch, id).map_or_else(|| "?".into(), |p| tr(p.name).to_string()),
            lane => lane.name(),
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

        let id = wid("pr-ev-lane", PianoLane::of_target(target).map_or(0, |(_, l)| l.key()), 0);
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

        // The curve, filled from its resting value (grey when muted).
        let def = self.project.automation_lane(target, scope);
        let muted = def.is_some_and(|l| l.muted);
        let points = def.map(|l| l.points.clone()).unwrap_or_default();
        let ink = if muted { t.muted } else { t.accent };
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
                c.fill_path(&fill, ink.alpha(0.16));
                c.stroke_polyline(&pts, 1.6, ink);
            }
            let zoom = self.piano.zoom;
            let dense = points.windows(2).any(|w| (w[1].tick - w[0].tick) as f32 * zoom < 5.0);
            if !dense {
                for p in &points {
                    let x = x_of(p.tick as f32);
                    if x >= lane.x - 4.0 && x <= lane.right() + 4.0 {
                        c.fill_circle(x, y_of(p.value), 2.6, if muted { t.muted } else { t.accent_hot });
                    }
                }
            }
            // The playhead on the curve.
            if let Some(tick) = self.live_tick(scope)
                && let Some(v) = ponzik_engine::sequence::automation_value(&points, tick)
            {
                let x = x_of(tick as f32);
                if x >= lane.x && x <= lane.right() {
                    c.fill_circle(x, y_of(v), 4.0, t.text);
                    c.stroke_circle(x, y_of(v), 4.0, 1.2, ink);
                }
            }
        }
        // Its name, in a corner, and whether it plays.
        let name = self.lane_short_name(target);
        let name = if muted { format!("{name} · {}", tr("muted")) } else { name };
        let nw = self.fonts.measure(self.f.sans_med, 10.0, &name) + 12.0;
        let tag = Rect::new(lane.x + 6.0, lane.y + 4.0, nw.min(lane.w * 0.5), 16.0);
        c.fill_rrect(tag, 5.0, t.bg_deep.alpha(0.6));
        self.text_fit(c, self.f.sans_med, 10.0, tag.x + 6.0, tag.y + 8.0, &name, tag.w - 10.0, if muted { t.muted } else { t.text2 });
        c.set_clip(Some(prev));
        let flash = self.curve_flash(target, scope);
        if flash > 0.0 {
            c.stroke_rrect(lane, 10.0, 2.0, t.select.alpha(flash));
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::app::View;

    fn app() -> App {
        let project = ponzik_project::demo_project();
        let none = |_: &str| None;
        let (_engine, handle) = project.build_engine(48_000.0, &none);
        App::new(handle, project, 48_000)
    }

    /// A short curve of `target` in `scope`.
    fn draw(app: &mut App, target: AutoTarget, scope: AutoScope) {
        app.project.add_automation_point(target, scope, 0, 0.2);
        app.project.add_automation_point(target, scope, 96, 0.8);
    }

    #[test]
    fn chips_list_the_curves_of_the_channel_in_the_pattern() {
        let mut app = app();
        let (pat, ch) = (app.project.current_pattern, 1);
        let here = AutoScope::Pattern(pat);
        draw(&mut app, AutoTarget::InstrumentParam { channel: ch, id: 3 }, here);
        draw(&mut app, AutoTarget::Cc { channel: ch, cc: 74 }, here);
        draw(&mut app, AutoTarget::PitchBend(ch), here);
        // Not chips of this channel in this pattern: another channel, another pattern, the song.
        draw(&mut app, AutoTarget::Cc { channel: 0, cc: 1 }, here);
        draw(&mut app, AutoTarget::Cc { channel: ch, cc: 1 }, AutoScope::Pattern(pat + 1));
        draw(&mut app, AutoTarget::InstrumentParam { channel: ch, id: 4 }, AutoScope::Song);
        app.piano.lane = PianoLane::Chance;
        assert_eq!(app.drawn_lanes(ch), [PianoLane::Param(3), PianoLane::Cc(74), PianoLane::PitchBend]);
        assert_eq!(app.other_channel_curves(ch), 1);
        // The controller lane shown is a chip too, even empty.
        app.piano.lane = PianoLane::Cc(11);
        assert_eq!(app.drawn_lanes(ch).last(), Some(&PianoLane::Cc(11)));
        // Every lane of a chip has its own widgets.
        let keys: Vec<usize> = app.drawn_lanes(ch).iter().map(|l| l.key()).collect();
        assert!(keys.iter().enumerate().all(|(i, k)| !keys[i + 1..].contains(k)));
    }

    #[test]
    fn a_chips_cross_deletes_only_its_curve() {
        let mut app = app();
        let (pat, ch) = (app.project.current_pattern, 1);
        let attack = AutoTarget::InstrumentParam { channel: ch, id: 3 };
        draw(&mut app, attack, AutoScope::Pattern(pat));
        draw(&mut app, attack, AutoScope::Song);
        draw(&mut app, AutoTarget::Cc { channel: ch, cc: 74 }, AutoScope::Pattern(pat));
        app.commit("draw");
        app.piano.stack = vec![PianoLane::Param(3)];
        let before = app.project.automation.len();
        app.delete_lane_curve(ch, PianoLane::Param(3));
        assert_eq!(app.project.automation.len(), before - 1);
        assert!(app.project.automation_lane(attack, AutoScope::Pattern(pat)).is_none());
        assert!(app.project.automation_lane(attack, AutoScope::Song).is_some(), "the song curve stays");
        assert!(app.project.automation_lane(AutoTarget::Cc { channel: ch, cc: 74 }, AutoScope::Pattern(pat)).is_some());
        assert!(app.piano.stack.is_empty());
        // Undo brings it back.
        app.undo();
        assert!(app.project.automation_lane(attack, AutoScope::Pattern(pat)).is_some());
    }

    #[test]
    fn show_curve_opens_its_view_pattern_channel_and_lane() {
        let mut app = app();
        assert!(app.project.patterns.len() > 1 && app.project.channels.len() > 2);
        let target = AutoTarget::InstrumentParam { channel: 2, id: 5 };
        draw(&mut app, target, AutoScope::Pattern(1));
        app.set_view(View::Mixer);
        app.show_curve(target, AutoScope::Pattern(1));
        assert_eq!(app.view, View::Piano);
        assert_eq!(app.project.current_pattern, 1);
        assert_eq!(app.piano_channel(), Some(2));
        assert_eq!(app.piano.lane, PianoLane::Param(5));
        assert!(app.curve_flash(target, AutoScope::Pattern(1)) > 0.0);
        // A CC and the pitch bend have their lanes too.
        let cc = AutoTarget::Cc { channel: 0, cc: 74 };
        draw(&mut app, cc, AutoScope::Pattern(0));
        app.show_curve(cc, AutoScope::Pattern(0));
        assert_eq!((app.project.current_pattern, app.piano_channel(), app.piano.lane), (0, Some(0), PianoLane::Cc(74)));
        // A song curve: the playlist.
        let gain = AutoTarget::TrackGain(1);
        draw(&mut app, gain, AutoScope::Song);
        app.show_curve(gain, AutoScope::Song);
        assert_eq!(app.view, View::Playlist);
    }

    #[test]
    fn the_list_mutes_and_deletes_one_curve() {
        let mut app = app();
        let info = *params_of(&app.project.channels[1].instrument.kind, true).iter().find(|p| p.max > p.min).expect("a parameter");
        let a = AutoTarget::InstrumentParam { channel: 1, id: info.id };
        draw(&mut app, a, AutoScope::Pattern(0));
        draw(&mut app, a, AutoScope::Song);
        let rows = app.curve_rows();
        assert_eq!(rows.iter().filter(|r| r.target == a).count(), 2);
        assert_eq!(rows.iter().position(|r| r.target == a && r.scope == AutoScope::Song).map(|i| i < rows.len()), Some(true));
        app.mute_curve(a, Some(AutoScope::Pattern(0)), true);
        assert!(app.project.automation_lane(a, AutoScope::Pattern(0)).unwrap().muted);
        assert!(!app.project.automation_lane(a, AutoScope::Song).unwrap().muted);
        // The knob's value: the curves go flat at it, points kept.
        let knob = info.from_norm(0.5);
        app.project.channels[1].instrument.set(info.id, knob);
        app.flatten_curve(a);
        assert!(app.project.automation.iter().filter(|l| l.target == a).all(|l| l.points.iter().all(|p| p.value == knob)));
        app.delete_curve(a, Some(AutoScope::Song));
        assert_eq!(app.curves_of(a), [(AutoScope::Pattern(0), true)]);
        // Menu: show it, play it again, reset, delete.
        let items = app.curve_menu_items(a);
        assert!(matches!(items[0].action, Action::ShowCurve(t, AutoScope::Pattern(0)) if t == a));
        assert!(items.iter().any(|i| matches!(i.action, Action::MuteCurve(_, None, false))));
    }
}
