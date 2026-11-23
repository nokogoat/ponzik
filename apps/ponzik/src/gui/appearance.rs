//! Settings › Appearance: the theme list and a visual theme editor (grouped
//! tokens, color picker, sliders, contrast check), applied live to the whole
//! interface. Edits are written into the theme's CSS file, in place.

use ponzik_i18n::{tr, tr_fmt};
use ponzik_ui::{Canvas, Color, Rect};
use ponzik_window::{Cursor, Key};

use super::Input;
use super::app::App;
use super::theme::*;
use super::themecss::value_text;
use super::themes::Source;
use super::widgets::{Icon, draw_icon, wid};

const LIST_W: f32 = 204.0;
const GROUPS_W: f32 = 176.0;
const PICKER_W: f32 = 250.0;
const ROW: f32 = 30.0;

/// Damier sous les couleurs transparentes.
fn checker(c: &mut Canvas, r: Rect) {
    c.fill_rect(r, Color::hex(0xd8d8d8));
    let s = 5.0;
    let mut y = r.y;
    let mut row = 0;
    while y < r.bottom() {
        let mut x = r.x + if row % 2 == 0 { 0.0 } else { s };
        while x < r.right() {
            c.fill_rect(Rect::new(x, y, s.min(r.right() - x), s.min(r.bottom() - y)), Color::hex(0x9a9a9a));
            x += 2.0 * s;
        }
        y += s;
        row += 1;
    }
}

impl App {
    pub(super) fn appearance_tab(&mut self, c: &mut Canvas, input: &Input, body: Rect, hint: &mut Option<String>) {
        if !self.themes.loaded {
            self.themes.load_user();
        }
        // The end of a drag writes the file once.
        if !input.down && self.themes.editor.dirty {
            if let Err(e) = self.themes.flush() {
                self.toast(&tr_fmt!("Could not save the theme: {}", e.to_string()));
            }
        }
        let list = Rect::new(body.x, body.y, LIST_W, body.h);
        self.theme_list(c, input, list, hint);
        let x = list.right() + 16.0;
        let editor = Rect::new(x, body.y, body.right() - x, body.h);
        self.theme_editor(c, input, editor, hint);
    }

    // --- Liste des thèmes ---

    fn theme_list(&mut self, c: &mut Canvas, input: &Input, r: Rect, hint: &mut Option<String>) {
        let t = self.theme;
        self.text(c, self.f.sans_med, 11.0, r.x + 2.0, r.y + 8.0, tr("Themes"), t.muted);
        let buttons_h = 76.0;
        let list = Rect::new(r.x, r.y + 22.0, r.w, r.h - 22.0 - buttons_h);
        c.fill_rrect(list, 12.0, t.well);
        let n = self.themes.entries.len();
        let content = n as f32 * 34.0 + 8.0;
        let max_scroll = (content - list.h).max(0.0);
        if input.inside && list.contains(input.x, input.y) && input.wheel != 0.0 {
            self.themes.editor.list_scroll = (self.themes.editor.list_scroll - input.wheel * 40.0).clamp(0.0, max_scroll);
        }
        let scroll = self.themes.editor.list_scroll.min(max_scroll);
        let prev = c.push_clip(list.inset(2.0));
        let mut chosen = None;
        for i in 0..n {
            let row = Rect::new(list.x + 4.0, list.y + 4.0 + i as f32 * 34.0 - scroll, list.w - 8.0, 32.0);
            if row.bottom() < list.y || row.y > list.bottom() {
                continue;
            }
            let e = &self.themes.entries[i];
            let (bg, panel, accent, select, text) = (e.set.root.bg, e.set.root.panel, e.set.root.accent, e.set.root.select, e.set.root.text);
            let locked = !e.editable();
            let broken = !e.errors.is_empty();
            let source = e.source.clone();
            let active = i == self.themes.active;
            let id = wid("th-row", i, 0);
            let hov = self.hovered(input, id, row) && list.contains(input.x, input.y);
            let hv = self.hover_anim(id, hov);
            let on = self.anim.to(id ^ 0x4f4e, active as u8 as f32, T_HOVER);
            c.fill_rrect(row, 9.0, t.raised.alpha(hv * 0.7).mix(t.raised_hi, on));
            // Miniature : fond, panneau, accent et sélection du thème.
            let sw = Rect::new(row.x + 6.0, row.y + 6.0, 30.0, 20.0);
            c.fill_rrect(sw, 5.0, bg);
            c.fill_rrect(Rect::new(sw.x + 10.0, sw.y, 20.0, 20.0), 5.0, panel);
            c.fill_circle(sw.x + 16.0, sw.y + 10.0, 4.0, accent);
            c.fill_circle(sw.x + 24.0, sw.y + 10.0, 2.5, select);
            c.fill_rect(Rect::new(sw.x + 3.0, sw.y + 6.0, 4.0, 1.5), text);
            c.stroke_rrect(sw, 5.0, 1.0, t.hairline);
            let label = self.theme_label(i);
            let right = if locked || broken { 22.0 } else { 6.0 };
            let col = if active { t.text } else { t.text2.mix(t.text, hv) };
            self.text_fit(c, self.f.sans_med, 12.5, row.x + 44.0, row.y + 16.0, &label, row.w - 44.0 - right, col);
            if broken {
                c.fill_circle(row.right() - 12.0, row.y + 16.0, 3.5, t.warning);
            } else if locked {
                draw_icon(c, Icon::Key, row.right() - 12.0, row.y + 16.0, 11.0, t.muted.alpha(0.7));
            }
            if hov {
                let kind = match source {
                    Source::Builtin(_) => tr("Built-in theme, read-only"),
                    Source::Example => tr("Example theme, read-only"),
                    Source::User(_) => tr("Your theme: edit it here or in its CSS file"),
                };
                *hint = Some(kind.to_string());
                if input.pressed && !active {
                    chosen = Some(i);
                }
            }
        }
        c.set_clip(Some(prev));
        if let Some(i) = chosen {
            self.set_theme(i);
        }
        // Actions sous la liste.
        let by = list.bottom() + 8.0;
        let half = (r.w - 8.0) * 0.5;
        if self.small_button(c, input, wid("th-dup", 0, 0), Rect::new(r.x, by, half, 30.0), Icon::Duplicate, tr("Duplicate")) {
            let i = self.themes.active;
            self.duplicate_theme(i);
        }
        if self.hovered(input, wid("th-dup", 0, 0), Rect::new(r.x, by, half, 30.0)) {
            *hint = Some(tr("A copy of this theme that you can edit").to_string());
        }
        let folder = Rect::new(r.x + half + 8.0, by, half, 30.0);
        if self.small_button(c, input, wid("th-folder", 0, 0), folder, Icon::Folder, tr("Folder")) {
            self.open_themes_folder();
        }
        if self.hovered(input, wid("th-folder", 0, 0), folder) {
            *hint = Some(tr_fmt!("Theme files: {}", self.themes.dir.display().to_string()));
        }
        self.text_fit(c, self.f.sans, 11.0, r.x + 2.0, by + 48.0, tr("Drop a .css theme on the window to import it"), r.w - 4.0, t.muted);
    }

    /// Bouton compact : pictogramme et mot.
    fn small_button(&mut self, c: &mut Canvas, input: &Input, id: u64, r: Rect, icon: Icon, label: &str) -> bool {
        let t = self.theme;
        let hov = self.hovered(input, id, r);
        let hv = self.hover_anim(id, hov);
        let press = self.anim.to(id ^ 0x5052, if hov && input.down { 0.96 } else { 1.0 }, T_PRESS);
        let b = r.scale(press);
        c.fill_rrect(b, 8.0, t.raised.mix(t.raised_hi, hv));
        let ink = t.text2.mix(t.text, hv);
        let w = self.fonts.measure(self.f.sans_med, 12.0, label) + 20.0;
        let x0 = b.x + ((b.w - w) * 0.5).max(6.0);
        let cy = b.y + b.h * 0.5;
        draw_icon(c, icon, x0 + 6.0, cy, 13.0, ink);
        self.text_fit(c, self.f.sans_med, 12.0, x0 + 18.0, cy, label, b.right() - x0 - 22.0, ink);
        hov && input.pressed
    }

    // --- Éditeur ---

    fn theme_editor(&mut self, c: &mut Canvas, input: &Input, r: Rect, hint: &mut Option<String>) {
        let t = self.theme;
        let i = self.themes.active;
        let entry = self.themes.entries[i].clone();
        let editable = entry.editable();
        let errors = entry.errors.clone();
        let base_label = self.themes.find(&entry.base_id).map(|b| self.theme_label(b)).unwrap_or_default();
        let name = self.theme_label(i);

        // En-tête : nom, origine, actions.
        self.text_fit(c, self.f.sans_med, 17.0, r.x, r.y + 10.0, &name, r.w - 230.0, t.text);
        let sub = match &entry.source {
            Source::Builtin(_) => tr("Built-in · read-only: changing a color makes an editable copy").to_string(),
            Source::Example => tr_fmt!("Example · based on {} · read-only: changing a color makes an editable copy", base_label),
            Source::User(p) => tr_fmt!("Based on {} · {}", base_label, p.display().to_string()),
        };
        self.text_fit(c, self.f.sans, 11.5, r.x, r.y + 30.0, &sub, r.w - 230.0, t.muted);
        let mut bx = r.right() - 32.0;
        let mut actions: Vec<(Icon, &str, u8)> = vec![(Icon::Export, tr("Export to share"), 2)];
        if editable {
            actions.push((Icon::Pencil, tr("Open the CSS file in your text editor (changes apply live)"), 1));
            actions.push((Icon::Trash, tr("Delete this theme"), 3));
        }
        actions.push((Icon::Dice, tr("Randomize the accent"), 4));
        actions.push((Icon::Theme, tr("Generate a whole theme from the accent color (readable contrast)"), 5));
        for (icon, tip, what) in actions {
            let b = Rect::new(bx, r.y + 4.0, 32.0, 32.0);
            let armed = what == 3 && self.themes.editor.delete_armed.as_deref() == Some(self.themes.active_id());
            let tip = if armed { tr("Click again to delete") } else { tip };
            if self.icon_button(c, input, wid("th-act", what as usize, 0), b, icon, armed, (tip, "")) {
                self.theme_action(what);
            }
            bx -= 36.0;
        }

        let top = r.y + 50.0;
        // Groupes de jetons.
        let groups = Rect::new(r.x, top, GROUPS_W, r.bottom() - top);
        let gsel = self.themes.editor.group.min(Group::ALL.len() - 1);
        for (k, g) in Group::ALL.iter().enumerate() {
            let row = Rect::new(groups.x, groups.y + k as f32 * 30.0, groups.w, 28.0);
            let id = wid("th-group", k, 0);
            let hov = self.hovered(input, id, row);
            let hv = self.hover_anim(id, hov);
            let on = self.anim.to(id ^ 0x4f4e, (k == gsel) as u8 as f32, T_HOVER);
            c.fill_rrect(row, 8.0, t.raised.alpha(hv * 0.6).mix(t.raised_hi, on));
            if on > 0.01 {
                c.fill_rrect(Rect::new(row.x + 2.0, row.y + 8.0, 3.0, row.h - 16.0), 1.5, t.accent.alpha(on));
            }
            // Échantillon des couleurs du groupe.
            let cols: Vec<Color> = TOKENS
                .iter()
                .enumerate()
                .filter(|(_, d)| d.group == *g && d.kind == Kind::Color)
                .take(3)
                .filter_map(|(j, _)| if let Val::Color(col) = t.get(j) { Some(col) } else { None })
                .collect();
            for (n, col) in cols.iter().enumerate() {
                let dot = Rect::new(row.right() - 10.0 - (cols.len() - n) as f32 * 9.0, row.y + 9.0, 10.0, 10.0);
                c.fill_circle(dot.x + 5.0, dot.y + 5.0, 5.0, *col);
                c.stroke_circle(dot.x + 5.0, dot.y + 5.0, 5.0, 1.0, t.hairline);
            }
            let ink = t.text2.mix(t.text, hv.max(on));
            self.text_fit(c, self.f.sans_med, 12.0, row.x + 12.0, row.y + 14.0, tr(g.label()), row.w - 12.0 - 12.0 - cols.len() as f32 * 9.0, ink);
            if hov && input.pressed {
                self.themes.editor.group = k;
                self.themes.editor.scroll = 0.0;
            }
        }
        // Erreurs du fichier, sous les groupes.
        let ey = groups.y + Group::ALL.len() as f32 * 30.0 + 10.0;
        if !errors.is_empty() {
            let box_r = Rect::new(groups.x, ey, groups.w, (groups.bottom() - ey).min(18.0 + 16.0 * errors.len().min(5) as f32));
            c.fill_rrect(box_r, 8.0, t.warning.alpha(0.1));
            c.stroke_rrect(box_r, 8.0, 1.0, t.warning.alpha(0.5));
            self.text(c, self.f.sans_med, 11.0, box_r.x + 8.0, box_r.y + 10.0, &tr_fmt!("{} problems in the file", errors.len()), t.warning);
            for (k, e) in errors.iter().take(5).enumerate() {
                let line = tr_fmt!("line {}: {}", e.line, e.msg.clone());
                let y = box_r.y + 26.0 + k as f32 * 16.0;
                if y > box_r.bottom() - 4.0 {
                    break;
                }
                self.text_fit(c, self.f.mono, 10.0, box_r.x + 8.0, y, &line, box_r.w - 14.0, t.text2);
                if input.inside && Rect::new(box_r.x, y - 8.0, box_r.w, 16.0).contains(input.x, input.y) {
                    *hint = Some(line);
                }
            }
        }

        // Jetons du groupe.
        let picker_x = r.right() - PICKER_W;
        let tokens = Rect::new(groups.right() + 12.0, top, picker_x - 16.0 - groups.right() - 12.0, r.bottom() - top);
        self.token_list(c, input, tokens, Group::ALL[gsel], editable, hint);
        let picker = Rect::new(picker_x, top, PICKER_W, r.bottom() - top);
        self.picker_column(c, input, picker, hint);
    }

    fn theme_action(&mut self, what: u8) {
        let i = self.themes.active;
        match what {
            1 => self.open_theme_file(i),
            2 => self.export_theme(i),
            3 => {
                let id = self.themes.active_id().to_string();
                if self.themes.editor.delete_armed.as_deref() == Some(id.as_str()) {
                    self.themes.editor.delete_armed = None;
                    self.delete_theme(i);
                } else {
                    self.themes.editor.delete_armed = Some(id);
                }
            }
            4 => {
                let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(7, |d| d.as_nanos() as u64);
                let accent = random_accent(seed);
                if let Some(k) = token_index("accent") {
                    self.edit_token(k, Some(Val::Color(accent)), true, true);
                }
            }
            _ => {
                let light = self.theme.is_light();
                let generated = generate_from_accent(self.theme.accent, light);
                let Some(k) = self.editable_theme() else { return };
                let values: Vec<(&str, Val)> = generated.iter().map(|&(n, col)| (n, Val::Color(col))).collect();
                if let Err(e) = self.themes.edit_many(k, &values) {
                    self.toast(&tr_fmt!("Could not save the theme: {}", e.to_string()));
                }
                self.theme = self.themes.set.root;
                self.toast(tr("Theme generated from the accent · undo per color"));
            }
        }
        if what != 3 {
            self.themes.editor.delete_armed = None;
        }
    }

    fn token_list(&mut self, c: &mut Canvas, input: &Input, r: Rect, group: Group, editable: bool, hint: &mut Option<String>) {
        let t = self.theme;
        let rows: Vec<usize> = TOKENS.iter().enumerate().filter(|(_, d)| d.group == group).map(|(i, _)| i).collect();
        let row_h = |k: Kind| if k == Kind::Color { ROW } else { 46.0 };
        let content: f32 = rows.iter().map(|&i| row_h(TOKENS[i].kind) + 2.0).sum();
        let max_scroll = (content - r.h).max(0.0);
        if input.inside && r.contains(input.x, input.y) && input.wheel != 0.0 {
            self.themes.editor.scroll = (self.themes.editor.scroll - input.wheel * 40.0).clamp(0.0, max_scroll);
        }
        let scroll = self.themes.editor.scroll.min(max_scroll);
        let prev = c.push_clip(r);
        let active = self.themes.active;
        let mut y = r.y - scroll;
        for i in rows {
            let def = &TOKENS[i];
            let h = row_h(def.kind);
            let row = Rect::new(r.x, y, r.w, h);
            y += h + 2.0;
            if row.bottom() < r.y || row.y > r.bottom() {
                continue;
            }
            let declared = self.themes.entries[active].declared(def.name).is_some();
            let can_undo = self.themes.can_undo(active, i);
            let selected = self.themes.editor.token == Some(i);
            let id = wid("th-tok", i, 0);
            let hov = self.hovered(input, id, row) && r.contains(input.x, input.y);
            let hv = self.hover_anim(id, hov);
            let sel = self.anim.to(id ^ 0x5345, selected as u8 as f32, T_HOVER);
            c.fill_rrect(row, 8.0, t.raised.alpha(0.35 * hv).mix(t.raised_hi, sel));
            // Undo and reset buttons on the right.
            let mut right = row.right() - 4.0;
            let cy = row.y + if def.kind == Kind::Color { h * 0.5 } else { 14.0 };
            if declared && editable {
                let b = Rect::new(right - 24.0, cy - 12.0, 24.0, 24.0);
                if self.icon_button(c, input, wid("th-reset", i, 0), b, Icon::Reset, false, (tr("Back to the inherited value"), "")) {
                    self.edit_token(i, None, true, true);
                }
                right -= 26.0;
            }
            if can_undo && editable {
                let b = Rect::new(right - 24.0, cy - 12.0, 24.0, 24.0);
                if self.icon_button(c, input, wid("th-undo", i, 0), b, Icon::Undo, false, (tr("Undo the last change of this token"), "")) {
                    if let Err(e) = self.themes.undo(active, i) {
                        self.toast(&tr_fmt!("Could not save the theme: {}", e.to_string()));
                    }
                    self.theme = self.themes.set.root;
                }
                right -= 26.0;
            }
            let value = self.themes.set.root.get(i);
            let text_col = t.text2.mix(t.text, hv.max(sel));
            match (def.kind, value) {
                (Kind::Color, Val::Color(col)) => {
                    let sw = Rect::new(row.x + 6.0, row.y + 5.0, 34.0, h - 10.0);
                    let prev_clip = c.push_clip(sw);
                    checker(c, sw);
                    c.set_clip(Some(prev_clip));
                    c.fill_rrect(sw, 5.0, col);
                    c.stroke_rrect(sw, 5.0, 1.0, t.text.alpha(0.18));
                    let name_w = self.fonts.measure(self.f.mono_med, 12.0, def.name);
                    self.text(c, self.f.mono_med, 12.0, row.x + 48.0, cy, def.name, text_col);
                    if declared {
                        c.fill_circle(row.x + 48.0 + name_w + 7.0, cy, 2.5, t.accent);
                    }
                    self.text_right(c, self.f.mono, 11.0, right - 6.0, cy, &hex_string(col), t.muted);
                    if hov {
                        *hint = Some(if declared {
                            tr_fmt!("--{} · set by this theme · click to edit the color", def.name)
                        } else {
                            tr_fmt!("--{} · inherited · click to edit the color", def.name)
                        });
                        if input.pressed && !self.over_icon(input, row, right) {
                            self.themes.editor.token = Some(i);
                            self.themes.editor.hex = None;
                            self.themes.editor.hsv = None;
                        }
                    }
                }
                (kind, Val::Num(v)) => {
                    self.text(c, self.f.mono_med, 12.0, row.x + 8.0, cy, def.name, text_col);
                    if declared {
                        let w = self.fonts.measure(self.f.mono_med, 12.0, def.name);
                        c.fill_circle(row.x + 8.0 + w + 7.0, cy, 2.5, t.accent);
                    }
                    self.text_right(c, self.f.mono, 11.0, right - 6.0, cy, &value_text(kind, value), t.muted);
                    let track = Rect::new(row.x + 8.0, row.y + 30.0, row.w - 16.0, 12.0);
                    let new = if kind == Kind::Density { self.density_choice(c, input, track, v) } else { self.token_slider(c, input, wid("th-slide", i, 0), track, v, def.range) };
                    if let Some(nv) = new {
                        let start = !self.themes.editor.dirty || !input.down;
                        self.edit_token(i, Some(Val::Num(nv)), start, !input.down || kind == Kind::Density);
                    }
                    if hov {
                        *hint = Some(match def.name {
                            "radius-sm" | "radius-md" | "radius-lg" => tr("Corner radius of small, medium and large shapes"),
                            "border-width" => tr("Thickness of outlines"),
                            "shadow-strength" => tr("Darkness of shadows under menus and windows (0: none)"),
                            "font-scale" => tr("Size of all text"),
                            "density" => tr("Spacing of lists, rows and mixer strips"),
                            _ => tr("Speed of animations (0: no animations)"),
                        }
                        .to_string());
                    }
                }
                _ => {}
            }
        }
        c.set_clip(Some(prev));
    }

    fn over_icon(&self, input: &Input, row: Rect, right: f32) -> bool {
        input.x > right && input.x <= row.right()
    }

    /// Glissière d'un jeton numérique ; double-clic : valeur du thème de base.
    fn token_slider(&mut self, c: &mut Canvas, input: &Input, id: u64, r: Rect, v: f32, (lo, hi): (f32, f32)) -> Option<f32> {
        let t = self.theme;
        let hov = self.hovered(input, id, r.inset(-4.0));
        if hov && input.pressed {
            self.begin_drag(id, input, v);
        }
        let mut out = None;
        if self.is_dragging(id) {
            self.set_cursor(Cursor::ResizeH);
            let f = ((input.x - r.x) / r.w).clamp(0.0, 1.0);
            let step = if hi - lo > 8.0 { 1.0 } else { 0.05 };
            let nv = ((lo + f * (hi - lo)) / step).round() * step;
            if (nv - v).abs() > 1e-4 {
                out = Some(nv);
            }
        }
        let hv = self.hover_anim(id, hov || self.is_dragging(id));
        let f = ((v - lo) / (hi - lo)).clamp(0.0, 1.0);
        let cy = r.y + r.h * 0.5;
        let bar = Rect::new(r.x, cy - 2.5, r.w, 5.0);
        c.fill_rrect(bar, 2.5, t.well.mix(t.raised_hi, 0.5 + 0.5 * hv));
        c.fill_rrect(Rect::new(bar.x, bar.y, bar.w * f, bar.h), 2.5, t.accent.alpha(0.6 + 0.4 * hv));
        c.fill_circle(r.x + f * r.w, cy, 6.5 + hv, t.text);
        out
    }

    fn density_choice(&mut self, c: &mut Canvas, input: &Input, r: Rect, v: f32) -> Option<f32> {
        let opts = [tr("Compact"), tr("Normal"), tr("Comfortable")];
        let values = [DENSITY_COMPACT, 1.0, DENSITY_COMFORTABLE];
        let sel = values.iter().position(|x| (x - v).abs() < 0.01).unwrap_or(1);
        let tips = [("", ""), ("", ""), ("", "")];
        let seg = Rect::new(r.x, r.y - 6.0, r.w, 24.0);
        self.segmented_tips(c, input, wid("th-density", 0, 0), seg, &opts, &tips, sel).map(|k| values[k]).filter(|x| (x - v).abs() > 1e-4)
    }

    // --- Sélecteur de couleur et contraste ---

    fn picker_column(&mut self, c: &mut Canvas, input: &Input, r: Rect, hint: &mut Option<String>) {
        let t = self.theme;
        let token = self.themes.editor.token.filter(|&i| TOKENS[i].kind == Kind::Color);
        let mut y = r.y;
        match token {
            Some(i) => {
                let Val::Color(col) = self.themes.set.root.get(i) else { return };
                self.text(c, self.f.mono_med, 12.0, r.x, y + 8.0, &format!("--{}", TOKENS[i].name), t.text);
                y += 22.0;
                if let Some(new) = self.color_picker(c, input, Rect::new(r.x, y, r.w, 230.0), col, hint) {
                    let start = !self.themes.editor.dirty;
                    self.edit_token(i, Some(Val::Color(new)), start, !input.down);
                }
                y += 244.0;
            }
            None => {
                let box_r = Rect::new(r.x, y, r.w, 64.0);
                c.fill_rrect(box_r, 10.0, t.well);
                draw_icon(c, Icon::Theme, box_r.x + 22.0, box_r.y + 32.0, 18.0, t.accent);
                self.text_fit(c, self.f.sans, 12.0, box_r.x + 42.0, box_r.y + 24.0, tr("Pick a color in the list"), box_r.w - 50.0, t.text2);
                self.text_fit(c, self.f.sans, 11.0, box_r.x + 42.0, box_r.y + 42.0, tr("Every change shows live in the app"), box_r.w - 50.0, t.muted);
                y += 76.0;
            }
        }
        // Contraste des textes sur leurs fonds (WCAG).
        let issues = contrast_issues(&self.themes.set.root);
        self.text(c, self.f.sans_med, 11.0, r.x + 2.0, y + 8.0, tr("Contrast check"), t.muted);
        y += 20.0;
        if issues.is_empty() {
            draw_icon(c, Icon::Check, r.x + 9.0, y + 9.0, 13.0, t.accent);
            self.text_fit(c, self.f.sans, 12.0, r.x + 22.0, y + 9.0, tr("All text is readable on its surface"), r.w - 22.0, t.text2);
            return;
        }
        for (k, is) in issues.iter().enumerate() {
            let row = Rect::new(r.x, y + k as f32 * 24.0, r.w, 22.0);
            if row.bottom() > r.bottom() {
                break;
            }
            let fg = token_index(is.fg).map(|j| self.themes.set.root.get(j));
            let bg = token_index(is.bg).map(|j| self.themes.set.root.get(j));
            if let (Some(Val::Color(fg)), Some(Val::Color(bg))) = (fg, bg) {
                let chip = Rect::new(row.x, row.y + 1.0, 30.0, 20.0);
                c.fill_rrect(chip, 5.0, bg);
                self.text_center(c, self.f.sans_med, 11.0, chip.x + 15.0, chip.y + 10.0, "Aa", fg);
            }
            let bad = is.ratio < is.min * 0.75;
            let col = if bad { t.negative } else { t.warning };
            let label = format!("{} / {}", is.fg, is.bg);
            self.text_fit(c, self.f.mono, 11.0, row.x + 38.0, row.y + 11.0, &label, row.w - 38.0 - 70.0, t.text2);
            self.text_right(c, self.f.mono_med, 11.0, row.right(), row.y + 11.0, &format!("{:.1} < {:.1}", is.ratio, is.min), col);
            if input.inside && row.contains(input.x, input.y) {
                *hint = Some(tr_fmt!("--{} on --{}: contrast {:.1}:1, {:.1}:1 needed to read comfortably", is.fg, is.bg, is.ratio, is.min));
                if input.pressed {
                    self.themes.editor.token = token_index(is.fg);
                    self.themes.editor.hex = None;
                    self.themes.editor.hsv = None;
                    if let Some(g) = token_index(is.fg).and_then(|j| Group::ALL.iter().position(|g| *g == TOKENS[j].group)) {
                        self.themes.editor.group = g;
                    }
                }
            }
        }
    }

    /// Carré saturation × valeur, bande de teinte, bande d'opacité et champ hexadécimal.
    fn color_picker(&mut self, c: &mut Canvas, input: &Input, r: Rect, col: Color, hint: &mut Option<String>) -> Option<Color> {
        let t = self.theme;
        let (mut h, mut s, mut v) = self.themes.editor.hsv.unwrap_or_else(|| to_hsv(col));
        // The color changed elsewhere (undo, file): follow it.
        if from_hsv(h, s, v, col.a) != col {
            (h, s, v) = to_hsv(col);
        }
        let mut alpha = col.a;
        let mut out = None;
        let sq = Rect::new(r.x, r.y, r.w, 150.0);
        let hue = Rect::new(r.x, sq.bottom() + 10.0, r.w, 12.0);
        let alp = Rect::new(r.x, hue.bottom() + 8.0, r.w, 12.0);
        // Carré : colonnes en dégradé vertical (la valeur est linéaire en RVB).
        let cols = 64;
        let cw = sq.w / cols as f32;
        for k in 0..cols {
            let top = from_hsv(h, (k as f32 + 0.5) / cols as f32, 1.0, 255);
            let (x0, x1) = ((sq.x + k as f32 * cw).round(), (sq.x + (k + 1) as f32 * cw).round());
            c.fill_rrect_vgrad(Rect::new(x0, sq.y, x1 - x0, sq.h), 0.0, top, Color::hex(0x000000));
        }
        c.stroke_rrect(sq, 0.0, 1.0, t.hairline);
        let (px, py) = (sq.x + s * sq.w, sq.y + (1.0 - v) * sq.h);
        c.stroke_circle(px, py, 6.0, 2.0, if v > 0.6 && s < 0.5 { Color::hex(0x000000) } else { WHITE });
        // Teinte.
        let segs = 72;
        let sw = hue.w / segs as f32;
        for k in 0..segs {
            let (x0, x1) = ((hue.x + k as f32 * sw).round(), (hue.x + (k + 1) as f32 * sw).round());
            c.fill_rect(Rect::new(x0, hue.y, x1 - x0, hue.h), from_hsv((k as f32 + 0.5) / segs as f32 * 360.0, 1.0, 1.0, 255));
        }
        let hx = hue.x + h / 360.0 * hue.w;
        c.fill_rrect(Rect::new(hx - 2.5, hue.y - 3.0, 5.0, hue.h + 6.0), 2.0, WHITE);
        c.stroke_rrect(Rect::new(hx - 2.5, hue.y - 3.0, 5.0, hue.h + 6.0), 2.0, 1.0, Color::rgba(0, 0, 0, 120));
        // Opacité.
        let prev = c.push_clip(alp);
        checker(c, alp);
        c.set_clip(Some(prev));
        for k in 0..32 {
            let a = (k as f32 + 0.5) / 32.0;
            let (x0, x1) = ((alp.x + k as f32 * alp.w / 32.0).round(), (alp.x + (k + 1) as f32 * alp.w / 32.0).round());
            c.fill_rect(Rect::new(x0, alp.y, x1 - x0, alp.h), Color::rgba(col.r, col.g, col.b, (a * 255.0) as u8));
        }
        let ax = alp.x + alpha as f32 / 255.0 * alp.w;
        c.fill_rrect(Rect::new(ax - 2.5, alp.y - 3.0, 5.0, alp.h + 6.0), 2.0, WHITE);
        c.stroke_rrect(Rect::new(ax - 2.5, alp.y - 3.0, 5.0, alp.h + 6.0), 2.0, 1.0, Color::rgba(0, 0, 0, 120));

        // Gestes.
        let (id_sq, id_hue, id_alp) = (wid("th-pick", 0, 0), wid("th-pick", 1, 0), wid("th-pick", 2, 0));
        for (id, zone) in [(id_sq, sq), (id_hue, hue.inset(-4.0)), (id_alp, alp.inset(-4.0))] {
            if self.hovered(input, id, zone) {
                self.set_cursor(Cursor::Hand);
                if input.pressed {
                    self.begin_drag(id, input, 0.0);
                }
            }
        }
        let fx = |z: Rect| ((input.x - z.x) / z.w).clamp(0.0, 1.0);
        if self.is_dragging(id_sq) {
            s = fx(sq);
            v = 1.0 - ((input.y - sq.y) / sq.h).clamp(0.0, 1.0);
        } else if self.is_dragging(id_hue) {
            h = fx(hue) * 359.9;
        } else if self.is_dragging(id_alp) {
            alpha = (fx(alp) * 255.0).round() as u8;
        }
        if self.is_dragging(id_sq) || self.is_dragging(id_hue) || self.is_dragging(id_alp) {
            self.themes.editor.hsv = Some((h, s, v));
            let new = from_hsv(h, s, v, alpha);
            if new != col {
                out = Some(new);
            }
        }

        // Champ hexadécimal, avec l'ancienne et la nouvelle couleur.
        let field = Rect::new(r.x + 40.0, alp.bottom() + 10.0, r.w - 40.0, 28.0);
        let chip = Rect::new(r.x, field.y, 32.0, 28.0);
        let prev = c.push_clip(chip);
        checker(c, chip);
        c.set_clip(Some(prev));
        c.fill_rrect(chip, 6.0, col);
        c.stroke_rrect(chip, 6.0, 1.0, t.hairline);
        let fid = wid("th-hex", 0, 0);
        let editing = self.themes.editor.hex.is_some();
        let hov = self.hovered(input, fid, field);
        if hov {
            self.set_cursor(Cursor::Text);
            *hint = Some(tr("Type a color: #22c55e, rgb(34 197 94), hsl(142 71% 45%)…").to_string());
            if input.pressed && !editing {
                self.themes.editor.hex = Some(hex_string(col));
            }
        } else if input.pressed && editing {
            self.commit_hex(&mut out);
        }
        if let Some(mut text) = self.themes.editor.hex.clone() {
            text.push_str(&input.text);
            let mut done = false;
            for &(key, _) in &input.keys {
                match key {
                    Key::Backspace => {
                        text.pop();
                    }
                    Key::Enter => done = true,
                    Key::Escape => {
                        self.themes.editor.hex = None;
                        return out;
                    }
                    _ => {}
                }
            }
            self.themes.editor.hex = Some(text);
            if done {
                self.commit_hex(&mut out);
            }
        }
        let editing = self.themes.editor.hex.is_some();
        let valid = self.themes.editor.hex.as_deref().is_none_or(|s| super::themecss::parse_color(s).is_ok());
        c.fill_rrect(field, 8.0, if editing { t.bg_deep } else { t.raised });
        c.stroke_rrect(field, 8.0, 1.0, if !valid { t.negative } else if editing { t.accent.alpha(0.7) } else { t.hairline });
        let shown = self.themes.editor.hex.clone().unwrap_or_else(|| hex_string(col));
        let w = self.text(c, self.f.mono_med, 13.0, field.x + 10.0, field.y + 14.0, &shown, t.text);
        if editing && (self.time * 2.0).fract() < 0.5 {
            c.fill_rect(Rect::new(field.x + 11.0 + w, field.y + 7.0, 1.5, 14.0), t.accent);
        }
        if editing {
            self.anim.set(wid("caret", 1, 0), self.time);
        }
        out
    }

    fn commit_hex(&mut self, out: &mut Option<Color>) {
        if let Some(text) = self.themes.editor.hex.take() {
            match super::themecss::parse_color(&text) {
                Ok(col) => {
                    self.themes.editor.hsv = None;
                    *out = Some(col);
                }
                Err(e) => self.toast(&e),
            }
        }
    }
}
