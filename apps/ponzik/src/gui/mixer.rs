//! Vue Mixer : master et pistes utilisées, inserts, pan, fader, VU-mètres.
//!
//! Bus : une piste dans laquelle d'autres sortent. Il s'affiche avant ses
//! pistes, qu'une barre de sa couleur relie sous les tranches, et peut les
//! replier sous lui. VCA : un fader sans son qui pilote le volume d'autres pistes.

use std::f32::consts::PI;

use ponzik_engine::mixer::{INSERT_SLOTS, MASTER, TRACKS};
use ponzik_engine::registry;
use ponzik_i18n::{num, tr_ctx, tr_fmt, tr_fmt_n};
use ponzik_project::TrackKind;
use ponzik_ui::{Canvas, Color, Rect};

use super::Input;
use super::app::{App, Focus, Meter, Popup};
use super::dnd::DropTarget;
use super::theme::*;
use super::widgets::{Icon, draw_icon, wid};

const STRIP_W: f32 = 112.0;
const STRIP_GAP: f32 = 8.0;
const DB_TICKS: [f32; 6] = [6.0, 0.0, -6.0, -12.0, -24.0, -48.0];

/// Position de fader (0..1) ↔ gain linéaire, loi cubique : 0 dB vers 79 %, +6 dB en haut.
pub fn pos_to_gain(pos: f32) -> f32 {
    2.0 * pos.clamp(0.0, 1.0).powi(3)
}

pub fn gain_to_pos(gain: f32) -> f32 {
    (gain.max(0.0) / 2.0).cbrt().min(1.0)
}

pub fn fmt_db(gain: f32) -> String {
    let d = 20.0 * gain.max(1e-9).log10();
    if d < -90.0 {
        return "−∞ dB".into();
    }
    let s = num(d.abs() as f64, 1);
    if d < -0.05 {
        format!("−{s} dB")
    } else if d > 0.05 {
        format!("+{s} dB")
    } else {
        format!("{} dB", num(0.0, 1))
    }
}

/// Pan lisible : « C », « G 30 », « D 45 ».
pub fn fmt_pan(pan: f32) -> String {
    match (pan * 100.0).round() as i32 {
        0 => tr_ctx("pan", "C").to_string(),
        p if p < 0 => tr_fmt!("L {}", -p),
        p => tr_fmt!("R {}", p),
    }
}

/// « −9,8 » : une décimale, virgule et vrai signe moins.
fn fmt_lufs(v: f32) -> String {
    let s = num(v.abs() as f64, 1);
    if v < -0.05 { format!("−{s}") } else { s }
}

fn fmt_tick(d: f32) -> String {
    match d {
        d if d > 0.0 => format!("+{d:.0}"),
        d if d < 0.0 => format!("−{:.0}", -d),
        _ => "0".into(),
    }
}

/// Couleur d'identité d'une piste (clés latérales, menus).
pub fn track_color(t: usize) -> Color {
    super::playlist::pattern_color(t.saturating_sub(1))
}

/// Place d'une tranche dans le mixer.
struct Slot {
    tr: usize,
    /// Nombre de bus au-dessus d'elle.
    depth: usize,
    /// Un bus parent est replié.
    hidden: bool,
}

/// Espace réservé sous les tranches pour la barre des groupes.
const GROUP_BAR: f32 = 10.0;

impl App {
    /// Couleur d'une piste : celle qu'on lui a donnée (bus, VCA), sinon celle de sa place.
    pub fn track_tint(&self, t: usize) -> Color {
        match self.project.tracks.get(t).map_or(0, |tr| tr.color) {
            0 => track_color(t),
            hex => Color::hex(hex),
        }
    }

    /// Bus dans lequel sort la piste, s'il y en a un.
    fn parent_bus(&self, t: usize) -> Option<usize> {
        let out = self.project.tracks[t].output;
        (t != MASTER && out != t && self.project.tracks[out].kind == TrackKind::Bus).then_some(out)
    }

    /// Pistes concernées par un geste de groupe sur `t` : la sélection si `t`
    /// en fait partie, sinon `t` seule.
    pub fn group_for(&self, t: usize) -> Vec<usize> {
        if self.mixer_sel.len() > 1 && self.mixer_sel.contains(&t) {
            let mut v = self.mixer_sel.clone();
            v.sort_unstable();
            v
        } else if t == MASTER {
            Vec::new()
        } else {
            vec![t]
        }
    }

    /// La nouvelle tranche glisse depuis la gauche et s'allume un instant.
    fn reveal_track(&mut self, t: usize) {
        self.anim.set(wid("mx-vis", t, 0), 0.0);
        self.anim.set(wid("mx-born", t, 0), 1.0);
        // Un VCA n'a pas d'effets à montrer dans le panneau du bas.
        if self.project.tracks[t].kind != TrackKind::Vca {
            self.focus = Focus::Track(t);
        }
        self.mixer_sel.clear();
    }

    /// Crée un bus et y fait sortir `members` (Ctrl+G, menus).
    pub fn new_bus(&mut self, members: &[usize]) {
        let audible = |m: &usize| *m != MASTER && self.project.tracks[*m].kind != TrackKind::Vca;
        let members: Vec<usize> = members.iter().copied().filter(audible).collect();
        let Some(bus) = self.project.add_bus(&members) else {
            self.toast(ponzik_i18n::tr("No free track left in the mixer"));
            return;
        };
        let refused = members.iter().filter(|&&m| self.project.tracks[m].output != bus).count();
        self.reveal_track(bus);
        self.commit(if members.is_empty() { ponzik_i18n::tr("New bus") } else { ponzik_i18n::tr("Group into a bus") });
        let name = self.project.tracks[bus].name.clone();
        let mut msg = tr_fmt!("{} created · double-click its name to rename it", name);
        if refused > 0 {
            msg = tr_fmt_n!("{} created · {} track left out: routing loop", "{} created · {} tracks left out: routing loop", refused, name, refused);
        }
        self.toast(&msg);
    }

    /// Bus de retour avec un effet réglé, et un envoi depuis `from`.
    pub fn new_return(&mut self, kind: &str, from: Option<usize>) {
        let Some(bus) = self.project.add_return(kind, from) else {
            self.toast(ponzik_i18n::tr("No free track left in the mixer"));
            return;
        };
        self.reveal_track(bus);
        self.panel = true;
        self.commit(ponzik_i18n::tr("New return bus"));
        let name = self.project.tracks[bus].name.clone();
        let msg = match from.filter(|&t| self.project.tracks[t].sends.iter().any(|s| s.0 == bus && s.1 > 0.0)) {
            Some(t) => tr_fmt!("{} created · sent from {}", name, self.project.tracks[t].name),
            None => tr_fmt!("{} created · use Send in a track's menu to feed it", name),
        };
        self.toast(&msg);
    }

    pub fn new_vca(&mut self, members: &[usize]) {
        let Some(vca) = self.project.add_vca(members) else {
            self.toast(ponzik_i18n::tr("No free track left in the mixer"));
            return;
        };
        self.reveal_track(vca);
        self.commit(ponzik_i18n::tr("New VCA"));
        let name = self.project.tracks[vca].name.clone();
        let n = self.project.vca_members(vca).len();
        let msg = if n == 0 {
            tr_fmt!("{} created · right-click a track › VCA to assign it", name)
        } else {
            tr_fmt_n!("{} controls {} track · no sound passes through it", "{} controls {} tracks · no sound passes through it", n, name, n)
        };
        self.toast(&msg);
    }

    pub fn toggle_fold(&mut self, bus: usize) {
        let folded = !self.project.tracks[bus].folded;
        self.project.tracks[bus].folded = folded;
        if folded {
            let hidden = self.project.bus_members(bus);
            self.mixer_sel.retain(|t| !hidden.contains(t));
        }
        self.commit(if folded { ponzik_i18n::tr("Fold a bus") } else { ponzik_i18n::tr("Unfold a bus") });
    }

    /// Défait un bus ou supprime un VCA (Ctrl+Z le rend).
    pub fn dissolve_track(&mut self, t: usize) {
        let kind = self.project.tracks[t].kind;
        if t == MASTER || kind == TrackKind::Normal {
            return;
        }
        if self.solo.is_some() {
            let s = self.solo.unwrap_or(MASTER);
            self.toggle_solo(s);
        }
        let name = self.project.tracks[t].name.clone();
        self.project.dissolve(t);
        self.mixer_sel.retain(|&x| x != t);
        if self.focus == Focus::Track(t) {
            self.focus = Focus::Track(self.project.tracks[t].output);
        }
        self.commit(if kind == TrackKind::Bus { ponzik_i18n::tr("Ungroup a bus") } else { ponzik_i18n::tr("Delete a VCA") });
        self.toast(&tr_fmt!("{} removed · Ctrl+Z to undo", name));
    }

    /// Inserts qui écoutent la piste `key` : (piste, nom de l'effet).
    fn key_listeners(&self, key: usize) -> Vec<(usize, &'static str)> {
        let mut out = Vec::new();
        for (t, tr) in self.project.tracks.iter().enumerate() {
            for fx in tr.inserts.iter().flatten().filter(|fx| fx.sidechain == Some(key)) {
                out.push((t, registry::effect_kind(&fx.kind).map_or(ponzik_i18n::tr("Effect"), |k| ponzik_i18n::tr(k.name))));
            }
        }
        out
    }

    /// Capture d'écran : bus, VCA, repli et sélection décrits par `PONZIK_MIXER`.
    pub fn screenshot_mixer(&mut self, spec: &str) {
        let list = |v: &str| v.split(',').filter_map(|x| x.trim().parse::<usize>().ok()).filter(|&x| x < TRACKS).collect::<Vec<_>>();
        let mut last = None;
        for op in spec.split(';').map(str::trim) {
            let (name, arg) = op.split_once(':').unwrap_or((op, ""));
            match name {
                "bus" => {
                    self.new_bus(&list(arg));
                    last = Some(self.focused_track());
                }
                "vca" => self.new_vca(&list(arg)),
                "fold" => {
                    if let Some(b) = last {
                        self.toggle_fold(b);
                    }
                }
                "sel" => self.mixer_sel = list(arg),
                "return" => {
                    let (kind, from) = arg.split_once('@').map_or((arg, None), |(k, f)| (k, f.trim().parse().ok()));
                    self.new_return(kind.trim(), from);
                }
                _ => {}
            }
        }
        // Les tranches arrivent déjà en place.
        for t in 0..TRACKS {
            self.anim.set(wid("mx-born", t, 0), 0.0);
            self.anim.set(wid("mx-vis", t, 0), 1.0);
        }
    }

    /// Pistes affichées : le master et celles qui servent (canaux, effets, bus, VCA…).
    fn visible_tracks(&self, all: bool) -> Vec<usize> {
        let mut out = vec![MASTER];
        out.extend((1..TRACKS).filter(|&t| all || self.project.track_in_use(t)));
        out
    }

    /// Ordre des tranches : chaque bus précède les pistes qui y sortent, et un
    /// groupe prend la place de sa piste la plus à gauche.
    fn mixer_layout(&self, all: bool) -> Vec<Slot> {
        let shown = self.visible_tracks(all);
        let parent = |t: usize| self.parent_bus(t).filter(|b| shown.contains(b));
        // Clé de tri : plus petit indice du sous-arbre.
        let mut key: Vec<usize> = (0..TRACKS).collect();
        for &t in &shown[1..] {
            let mut p = parent(t);
            for _ in 0..TRACKS {
                let Some(b) = p else { break };
                key[b] = key[b].min(t);
                p = parent(b);
            }
        }
        let children = |b: Option<usize>| {
            let mut v: Vec<usize> = shown[1..].iter().copied().filter(|&t| parent(t) == b).collect();
            v.sort_by_key(|&t| (key[t], t));
            v
        };
        let mut out = vec![Slot { tr: MASTER, depth: 0, hidden: false }];
        let mut stack: Vec<(usize, usize, bool)> = children(None).into_iter().rev().map(|t| (t, 0, false)).collect();
        while let Some((t, depth, hidden)) = stack.pop() {
            out.push(Slot { tr: t, depth, hidden });
            if depth < TRACKS && self.project.tracks[t].kind == TrackKind::Bus {
                let fold = hidden || self.project.tracks[t].folded;
                stack.extend(children(Some(t)).into_iter().rev().map(|c| (c, depth + 1, fold)));
            }
        }
        out
    }

    pub(super) fn mixer_view(&mut self, c: &mut Canvas, input: &Input, header: Rect, body: Rect) {
        let t = self.theme;
        let all_key = wid("mx-all", 0, 0);
        let all = self.anim.get(all_key).unwrap_or(0.0) > 0.5;
        let toggle = Rect::new(header.right() - 36.0, header.y, 36.0, header.h);
        let tip = if all { (ponzik_i18n::tr("Used tracks only"), "") } else { (ponzik_i18n::tr("All tracks"), "") };
        if self.icon_button(c, input, wid("mx-all-btn", 0, 0), toggle, Icon::Columns, all, tip) {
            self.anim.set(all_key, if all { 0.0 } else { 1.0 });
        }
        // « + Bus » : nouveau bus, retour d'effet ou VCA.
        let add = Rect::new(toggle.x - 8.0 - 76.0, header.y, 76.0, header.h);
        if self.mixer_add_button(c, input, add) {
            self.open_popup(Popup::NewBus, add);
        }
        self.mixer_sel.retain(|&s| s != MASTER && s < TRACKS);
        let n = self.mixer_sel.len();
        let selv = self.anim.to(wid("mx-sel-hint", 0, 0), (n > 0) as u8 as f32, T_HOVER);
        if selv > 0.01 {
            let label = tr_fmt_n!("{} track", "{} tracks", n, n);
            let right = add.x - 12.0;
            let x = self.keycap(c, right, header.y + header.h * 0.5, "Ctrl+G", selv);
            self.text_right(c, self.f.sans, 12.0, x - 8.0, header.y + header.h * 0.5, &tr_fmt!("{} · group", label), t.text2.alpha(selv));
        }

        c.fill_rrect(body, 16.0, t.panel);
        c.stroke_rrect(body, 16.0, 1.0, t.hairline);
        let inner = body.inset(12.0);
        let layout = self.mixer_layout(all);
        let target_w: f32 = layout.iter().filter(|s| !s.hidden).count() as f32 * (STRIP_W + STRIP_GAP) + 24.0;
        let content_w = target_w;
        if input.inside && inner.contains(input.x, input.y) && input.wheel != 0.0 && input.mods.shift {
            self.mixer_scroll = (self.mixer_scroll - input.wheel * 80.0).clamp(0.0, (content_w - inner.w).max(0.0));
        }
        // Bouton du milieu : faire glisser les tranches.
        let pan_id = wid("mx-pan-drag", 0, 0);
        if input.inside && inner.contains(input.x, input.y) && input.middle_pressed {
            self.pan = Some((pan_id, input.x, input.y, self.mixer_scroll, 0.0));
        }
        if let Some((_, x0, _, s0, _)) = self.pan.filter(|p| p.0 == pan_id) {
            self.mixer_scroll = s0 - (input.x - x0);
            self.anim.set(wid("mx-sx", 0, 0), self.mixer_scroll.clamp(0.0, (content_w - inner.w).max(0.0)));
        }
        self.mixer_scroll = self.mixer_scroll.clamp(0.0, (content_w - inner.w).max(0.0));
        let sx = self.anim.to(wid("mx-sx", 0, 0), self.mixer_scroll, T_LAYOUT);
        let prev = c.push_clip(inner);
        let strip_h = inner.h - GROUP_BAR;
        // Une tranche qui se replie glisse sous sa voisine de gauche.
        let masked = Input { inside: false, pressed: false, right_pressed: false, wheel: 0.0, double: false, ..input.clone() };
        let mut spans: Vec<(f32, f32)> = Vec::with_capacity(layout.len());
        let mut x = inner.x - sx;
        for (k, slot) in layout.iter().enumerate() {
            let vis = self.anim.to(wid("mx-vis", slot.tr, 0), if slot.hidden { 0.0 } else { 1.0 }, T_LAYOUT);
            let w = STRIP_W * vis;
            spans.push((x, x + w));
            if vis > 0.01 {
                let r = Rect::new(x - STRIP_W * (1.0 - vis), inner.y, STRIP_W, strip_h);
                // L'écart compris : un bus replié y montre ses tranches empilées.
                let clip = Rect::new(x, inner.y, w + STRIP_GAP * vis, strip_h);
                if clip.right() > inner.x && clip.x < inner.right() {
                    let inp = if vis > 0.98 { input } else { &masked };
                    let before = c.push_clip(clip);
                    self.mixer_strip(c, inp, r, slot.tr);
                    c.set_clip(Some(before));
                }
            }
            x += (STRIP_W + STRIP_GAP) * vis;
            if k == 0 {
                x += 12.0;
            }
        }
        // Barres des groupes : du bus à sa dernière piste visible.
        let bar_y = inner.y + strip_h + 4.0;
        for (k, slot) in layout.iter().enumerate() {
            if self.project.tracks[slot.tr].kind != TrackKind::Bus || spans[k].1 - spans[k].0 < 2.0 {
                continue;
            }
            let end = layout[k + 1..].iter().take_while(|s| s.depth > slot.depth).count();
            if end == 0 {
                continue;
            }
            let x1 = spans[k..=k + end].iter().map(|s| s.1).fold(spans[k].1, f32::max);
            let folded = self.project.tracks[slot.tr].folded;
            let a = if folded { 0.45 } else { 0.85 };
            c.fill_rrect(Rect::new(spans[k].0 + 4.0, bar_y, x1 - spans[k].0 - 8.0, 3.0), 1.5, self.track_tint(slot.tr).alpha(a));
        }
        c.set_clip(Some(prev));
    }

    /// Bouton « + Bus » de l'en-tête du mixer.
    fn mixer_add_button(&mut self, c: &mut Canvas, input: &Input, r: Rect) -> bool {
        let t = self.theme;
        let id = wid("mx-add-bus", 0, 0);
        let hov = self.hovered(input, id, r);
        let hv = self.hover_anim(id, hov);
        let press = self.anim.to(id ^ 0x5052, if hov && input.down { 0.95 } else { 1.0 }, T_PRESS);
        let rr = r.scale(press);
        c.fill_rrect(rr, 8.0, t.raised.alpha(0.5 + 0.5 * hv));
        c.stroke_rrect(rr, 8.0, 1.0, t.hairline);
        let ink = t.text2.mix(t.text, hv);
        let (cx, cy) = r.center();
        draw_icon(c, Icon::Bus, cx - 16.0, cy, 15.0, ink.mix(t.accent, hv));
        self.text(c, self.f.sans_med, 13.0, cx - 4.0, cy, ponzik_i18n::tr("Bus"), ink);
        if hov {
            self.set_tip(ponzik_i18n::tr("New bus, effect return or VCA\nCtrl+click strips to group them"), "Ctrl+G");
        }
        hov && input.pressed
    }

    fn mixer_strip(&mut self, c: &mut Canvas, input: &Input, r: Rect, tr: usize) {
        let t = self.theme;
        let kind = self.project.tracks[tr].kind;
        let tint = self.track_tint(tr);
        let focused = self.focused_track() == tr && matches!(self.focus, Focus::Track(_));
        let fv = self.anim.to(wid("mx-focus", tr, 0), focused as u8 as f32, T_HOVER);
        let selected = self.mixer_sel.contains(&tr);
        let sv = self.anim.to(wid("mx-sel", tr, 0), selected as u8 as f32, T_HOVER);
        let born = self.anim.to(wid("mx-born", tr, 0), 0.0, 0.35);
        // Bus replié : les tranches cachées dépassent comme des cartes empilées.
        let members = if kind == TrackKind::Bus { self.project.bus_members(tr).len() } else { 0 };
        let fold = self.anim.to(wid("mx-fold", tr, 0), self.project.tracks[tr].folded as u8 as f32, T_LAYOUT);
        if members > 0 && fold > 0.01 {
            for k in (1..=members.min(2)).rev() {
                let d = k as f32 * 3.0 * fold;
                let card = Rect::new(r.x + d, r.y + d * 1.5, r.w, r.h - d * 3.0);
                c.fill_rrect(card, 12.0, t.strip.mix(tint, 0.12).alpha(fold));
                c.stroke_rrect(card, 12.0, 1.0, tint.alpha(0.35 * fold));
            }
        }
        c.fill_rrect(r, 12.0, t.strip.mix(t.raised, fv * 0.6).mix(t.select, sv * 0.08));
        if kind != TrackKind::Normal {
            // Bandeau de la couleur du bus ou du VCA derrière le nom.
            let prev = c.push_clip(Rect::new(r.x, r.y, r.w, 42.0));
            c.fill_rrect(r, 12.0, tint.alpha(0.14));
            c.set_clip(Some(prev));
            c.fill_rect(Rect::new(r.x + 10.0, r.y + 42.0, r.w - 20.0, 1.0), tint.alpha(0.35));
        }
        let edge = if selected || focused { t.select.alpha(0.6 + 0.3 * sv) } else { t.hairline };
        c.stroke_rrect(r, 12.0, 1.0, edge.mix(tint, born));
        if born > 0.01 {
            c.stroke_rrect(r.inset(-1.0), 13.0, 2.0, tint.alpha(born * 0.8));
        }
        if kind != TrackKind::Vca {
            self.drop_zone(input, r, DropTarget::Track(tr));
        }
        let inner = r.inset(8.0);
        let parent = self.parent_bus(tr);

        // Nom : clic pour afficher la chaîne d'effets en bas.
        let name_r = Rect::new(inner.x, inner.y, inner.w, 28.0);
        let nid = wid("mx-name", tr, 0);
        let nhov = self.hovered(input, nid, name_r);
        // Pastille du bus de sortie, puis chevron de repli d'un bus.
        let mut left = 0.0;
        if let Some(bus) = parent {
            let (dx, dy) = (inner.x + 3.5, inner.y + 10.0);
            c.fill_circle(dx, dy, 3.5, self.track_tint(bus));
            if self.hovered(input, wid("mx-dot", tr, 0), Rect::new(dx - 6.0, dy - 7.0, 12.0, 14.0)) {
                let tip = tr_fmt!("Outputs to {}", self.project.tracks[bus].name);
                self.set_tip(&tip, "");
            }
            left = 10.0;
        }
        let chev = Rect::new(inner.x + left, inner.y + 1.0, 18.0, 18.0);
        let cid = wid("mx-chev", tr, 0);
        let chov = members > 0 && self.hovered(input, cid, chev);
        if members > 0 {
            let hv = self.hover_anim(cid, chov);
            if hv > 0.01 {
                c.fill_rrect(chev, 5.0, t.raised_hi.alpha(hv));
            }
            // Replié : la pointe montre où les pistes se déplient.
            let a = PI * (1.0 - fold);
            let (cx, cy) = chev.center();
            let pt = |x: f32, y: f32| (cx + x * a.cos() - y * a.sin(), cy + x * a.sin() + y * a.cos());
            c.stroke_polyline(&[pt(-2.0, -4.0), pt(2.0, 0.0), pt(-2.0, 4.0)], 1.6, tint.mix(t.text, hv * 0.4));
            if chov {
                let tip = if fold > 0.5 {
                    tr_fmt_n!("Unfold · {} track", "Unfold · {} tracks", members, members)
                } else {
                    tr_fmt_n!("Fold · {} track", "Fold · {} tracks", members, members)
                };
                self.set_tip(&tip, "");
                if input.pressed {
                    self.toggle_fold(tr);
                }
            }
            left += 20.0;
        }
        let nhov = nhov && !chov;
        if nhov && input.pressed && input.mods.ctrl && tr != MASTER {
            // Ctrl+clic : sélection multiple, à grouper (Ctrl+G).
            if selected {
                self.mixer_sel.retain(|&x| x != tr);
            } else {
                self.mixer_sel.push(tr);
            }
        } else if nhov && input.pressed {
            self.mixer_sel.clear();
            self.focus = Focus::Track(tr);
            self.panel = true;
            if input.double {
                let text = self.project.tracks[tr].name.clone();
                self.open_popup(Popup::Rename { target: super::app::Rename::Track(tr), text }, name_r);
            }
        }
        if nhov && input.right_pressed {
            self.focus = Focus::Track(tr);
            self.open_popup(Popup::TrackMenu(tr), Rect::new(input.x, input.y, 0.0, 0.0));
        }
        if nhov {
            self.set_tip(ponzik_i18n::tr("Double-click: rename · Ctrl+click: multi-select · right-click: menu"), "");
        }
        let name = self.project.tracks[tr].name.clone();
        // Centré, ou entre la pastille et l'armement quand la gauche est occupée.
        // The arm button keeps its own corner: the name never runs under it.
        let arm_w = if tr != MASTER && kind != TrackKind::Vca { 20.0 } else { 2.0 };
        let (x0, x1) = if left > 0.0 { (inner.x + left, inner.right() - arm_w) } else { (inner.x + arm_w, inner.right() - arm_w) };
        let (size, label) = self.fit_shrink(self.f.sans_med, 13.0, &name, x1 - x0);
        self.text_center(c, self.f.sans_med, size, (x0 + x1) * 0.5, inner.y + 10.0, &label, t.text);
        // Armement de l'enregistrement audio.
        if tr != MASTER && kind != TrackKind::Vca {
            let arm = Rect::new(inner.right() - 18.0, inner.y + 2.0, 18.0, 18.0);
            let aid = wid("mx-arm", tr, 0);
            let ahov = self.hovered(input, aid, arm);
            let armed = self.rec.is_armed(tr);
            let ahv = self.hover_anim(aid, ahov);
            let col = if armed { t.negative } else { t.muted.mix(t.text2, ahv) };
            if armed {
                c.fill_circle(arm.x + 9.0, arm.y + 9.0, 6.0, col);
            } else {
                draw_icon(c, Icon::Record, arm.x + 9.0, arm.y + 9.0, 13.0, col);
            }
            if ahov {
                self.set_tip(if armed { ponzik_i18n::tr("Disarm · right click: recording panel") } else { ponzik_i18n::tr("Arm for audio recording · right click: recording panel") }, "");
            }
            if ahov && input.pressed {
                self.toggle_arm(tr);
            }
            if ahov && input.right_pressed {
                self.open_record_panel(Some((arm.x + 9.0, arm.bottom() + 6.0)));
            }
        }
        let num = if tr == MASTER { tr_ctx("master track", "M").to_string() } else { tr.to_string() };
        let (cx, ny) = (inner.x + inner.w * 0.5, inner.y + 25.0);
        // Bus et VCA : leur pictogramme avant le numéro.
        let kind_icon = match kind {
            TrackKind::Bus => Some((Icon::Bus, ponzik_i18n::tr("Bus: other tracks output into it, its effects process their sum"))),
            TrackKind::Vca => Some((Icon::Vca, ponzik_i18n::tr("VCA: its fader controls the volume of its tracks, no sound passes through it"))),
            TrackKind::Normal => None,
        };
        if let Some((icon, tip)) = kind_icon {
            draw_icon(c, icon, cx - 12.0, ny, 12.0, tint);
            self.text(c, self.f.mono, 10.0, cx - 3.0, ny, &num, t.text2);
            if self.hovered(input, wid("mx-kind", tr, 0), Rect::new(cx - 22.0, ny - 7.0, 36.0, 14.0)) {
                self.set_tip(tip, "");
            }
        } else {
            self.text_center(c, self.f.mono, 10.0, cx, ny, &num, t.muted);
        }
        // Piste pilotée par un VCA : le pictogramme du VCA à gauche du numéro.
        if let Some(v) = self.project.tracks[tr].vca.filter(|&v| v < TRACKS) {
            let vx = cx - 16.0;
            draw_icon(c, Icon::Vca, vx, ny, 11.0, self.track_tint(v));
            if self.hovered(input, wid("mx-vca-tag", tr, 0), Rect::new(vx - 7.0, ny - 7.0, 14.0, 14.0)) {
                let tip = tr_fmt!("Controlled by {}", self.project.tracks[v].name);
                self.set_tip(&tip, "");
            }
        }
        // Piste qui sert de clé : une petite clé de sa couleur, à droite du numéro.
        let listeners = self.key_listeners(tr);
        if !listeners.is_empty() {
            let (kx, ky) = (inner.x + inner.w * 0.5 + 16.0, inner.y + 25.0);
            draw_icon(c, Icon::Key, kx, ky, 12.0, tint);
            let zone = Rect::new(kx - 8.0, ky - 7.0, 16.0, 14.0);
            if self.hovered(input, wid("mx-keysrc", tr, 0), zone) {
                let list: Vec<String> =
                    listeners.iter().map(|(t, n)| format!("{} · {n}", self.project.tracks[*t].name)).collect();
                self.set_tip(&tr_fmt!("Sidechain key for\n{}", list.join("\n")), "");
            }
        }

        if kind == TrackKind::Vca {
            self.vca_strip_body(c, input, inner, tr);
            return;
        }

        // Inserts.
        let mut y = inner.y + 38.0;
        for slot in 0..INSERT_SLOTS {
            let row = Rect::new(inner.x, y, inner.w, 20.0);
            let id = wid("mx-slot", tr, slot);
            let fx = self.project.tracks[tr].inserts[slot].clone();
            // Entrée latérale : un petit sélecteur à clé au bout de l'insert.
            let keyed = fx.as_ref().is_some_and(|d| registry::takes_sidechain(&d.kind));
            let key_r = Rect::new(row.right() - 19.0, row.y + 1.0, 18.0, 18.0);
            let kid = wid("mx-key", tr, slot);
            let khov = keyed && self.hovered(input, kid, key_r);
            let hov = self.hovered(input, id, row) && !khov;
            let hv = self.hover_anim(id, hov);
            self.drop_zone(input, row, DropTarget::Slot(tr, slot));
            match &fx {
                Some(def) => {
                    let name = registry::effect_kind(&def.kind).map_or(def.kind.as_str(), |k| ponzik_i18n::tr(k.name));
                    c.fill_rrect(row, 6.0, t.raised.mix(t.raised_hi, hv));
                    let dot = if def.bypass { t.muted } else { t.accent };
                    c.fill_circle(row.x + 9.0, row.y + 10.0, 3.0, dot);
                    let text_w = row.w - if keyed { 40.0 } else { 22.0 };
                    let ink = if def.bypass { t.muted } else { t.text2 };
                    self.text_fit(c, self.f.sans, 11.0, row.x + 18.0, row.y + 10.0, name, text_w, ink);
                    if keyed {
                        self.key_selector(c, input, kid, key_r, khov, tr, slot, def.sidechain);
                    }
                }
                None => {
                    c.stroke_rrect(row, 6.0, 1.0, t.hairline.alpha(0.6 + hv));
                    if hv > 0.01 {
                        draw_icon(c, Icon::Plus, row.x + row.w * 0.5, row.y + 10.0, 11.0, t.muted.mix(t.accent, hv).alpha(hv));
                    }
                }
            }
            if hov && input.pressed {
                self.focus = Focus::Track(tr);
                self.panel = true;
                if fx.is_none() {
                    self.open_popup(Popup::AddEffect { track: tr }, row);
                }
            }
            if hov {
                self.set_tip(if fx.is_some() { ponzik_i18n::tr("Right-click: remove") } else { ponzik_i18n::tr("Add an effect") }, "");
            }
            if hov && input.right_pressed && fx.is_some() {
                self.project.tracks[tr].inserts[slot] = None;
                self.commit(ponzik_i18n::tr("Remove an effect"));
            }
            y += 22.0;
        }

        // Envois actifs : destination et niveau. Hauteur fixe, pour que les
        // faders restent alignés d'une tranche à l'autre.
        y += 4.0;
        let sends_top = y;
        let mut shown = 0;
        for slot in 0..ponzik_engine::mixer::SEND_SLOTS {
            let (target, amount) = self.project.tracks[tr].sends[slot];
            if amount <= 0.0 || target == tr || shown == 2 {
                continue;
            }
            shown += 1;
            let row = Rect::new(inner.x, y, inner.w, 24.0);
            let label = format!("→ {}", self.project.tracks[target].name);
            self.text_fit(c, self.f.sans, 11.0, row.x + 2.0, row.y + 12.0, &label, row.w - 30.0, t.text2);
            let knob_c = (row.right() - 11.0, row.y + 12.0);
            let send = ponzik_project::midi_map::ControlTarget::Send { track: tr, slot };
            if input.right_pressed && self.hovered(input, wid("mx-send", tr, slot), Rect::new(knob_c.0 - 14.0, knob_c.1 - 14.0, 28.0, 28.0)) {
                self.open_popup(Popup::Control(send), Rect::new(input.x, input.y, 0.0, 0.0));
            }
            self.ctl_mark(c, knob_c, 11.0, send);
            if let Some(v) = self.mini_knob(c, input, wid("mx-send", tr, slot), knob_c, amount, false) {
                // Un envoi ramené à zéro se libère.
                self.project.tracks[tr].sends[slot].1 = if v < 0.01 { 0.0 } else { v };
                self.commit(ponzik_i18n::tr("Send level"));
            }
            y += 26.0;
        }
        if tr == MASTER {
            self.loudness_box(c, input, Rect::new(inner.x, sends_top, inner.w, 46.0));
        }
        y = sends_top + 52.0;

        // Pan.
        let pan_y = y + 20.0;
        let pan = self.project.tracks[tr].pan;
        if let Some(v) = self.mini_knob(c, input, wid("mx-pan", tr, 0), (inner.x + 18.0, pan_y), (pan + 1.0) * 0.5, true) {
            self.project.tracks[tr].pan = ((v * 2.0 - 1.0) * 100.0).round() / 100.0;
            self.commit(ponzik_i18n::tr("Track pan"));
        }
        self.knob_menu(input, (inner.x + 18.0, pan_y), 14.0, ponzik_engine::AutoTarget::TrackPan(tr));
        self.ctl_mark(c, (inner.x + 18.0, pan_y), 11.0, ponzik_project::midi_map::ControlTarget::Param(ponzik_engine::AutoTarget::TrackPan(tr)));
        let pan_text = fmt_pan(pan);
        self.text_right(c, self.f.mono, 11.0, inner.right(), pan_y, &pan_text, t.text2);
        self.tip_zone(input, Rect::new(inner.x, pan_y - 14.0, inner.w, 28.0), ponzik_i18n::tr("Pan"), "");

        // Bas : sortie, mute ; valeur au-dessus ; fader au milieu.
        let mute = Rect::new(inner.x, inner.bottom() - 26.0, inner.w, 26.0);
        let out_r = Rect::new(inner.x, mute.y - 28.0, inner.w, 24.0);
        let readout_y = out_r.y - 12.0;
        let fader = Rect::new(inner.x, pan_y + 22.0, inner.w, readout_y - 12.0 - pan_y - 22.0);
        if fader.h > 40.0 {
            self.track_fader(c, input, fader, tr);
        }
        let gain = self.project.tracks[tr].gain;
        self.text_center(c, self.f.mono, 12.0, inner.x + inner.w * 0.5, readout_y, &fmt_db(gain), t.text2);
        // CPU load of the track (inserts and fader), share of one core.
        let cpu = self.engine.track_cpu(tr);
        if cpu >= 0.0005 {
            self.text_right(c, self.f.mono, 9.0, inner.right(), pan_y + 16.0, &ponzik_i18n::tr_fmt!("{:.1}%", cpu * 100.0), t.muted);
        }

        let oid = wid("mx-out", tr, 0);
        let ohov = self.hovered(input, oid, out_r) && tr != MASTER;
        let ohv = self.hover_anim(oid, ohov);
        if ohv > 0.01 {
            c.fill_rrect(out_r, 6.0, t.raised.alpha(ohv));
        }
        if tr != MASTER {
            let out_label = self.project.tracks[self.project.tracks[tr].output].name.clone();
            let lw = self.fit(self.f.sans, 11.0, &out_label, out_r.w - 26.0);
            let tw = self.fonts.measure(self.f.sans, 11.0, &lw) + 16.0;
            let x0 = out_r.x + (out_r.w - tw) * 0.5;
            let ink = t.muted.mix(t.text2, ohv);
            let route_ink = parent.map_or(ink, |b| self.track_tint(b));
            draw_icon(c, Icon::Route, x0 + 5.0, out_r.y + 12.0, 11.0, route_ink);
            self.text(c, self.f.sans, 11.0, x0 + 16.0, out_r.y + 12.0, &lw, ink);
        }
        if ohov {
            self.set_tip(ponzik_i18n::tr("Output"), "");
        }
        if ohov && input.pressed {
            self.open_popup(Popup::Output(tr), out_r);
        }

        let muted = self.project.tracks[tr].muted;
        let half = (mute.w - 4.0) * 0.5;
        let mute_r = Rect::new(mute.x, mute.y, if tr == MASTER { mute.w } else { half }, mute.h);
        let mid = wid("mx-mute", tr, 0);
        let hov = self.hovered(input, mid, mute_r);
        if hov && input.pressed {
            self.project.tracks[tr].muted = !muted;
            self.commit(ponzik_i18n::tr("Track mute"));
        }
        let hv = self.hover_anim(mid, hov);
        let (fill, ink) = if muted { (t.text.mix(WHITE, hv * 0.2), t.bg_deep) } else { (t.raised.mix(t.raised_hi, hv), t.text2) };
        c.fill_rrect(mute_r, 8.0, fill);
        let (mx, my) = mute_r.center();
        draw_icon(c, Icon::Mute, mx, my, 14.0, ink);
        if hov {
            self.set_tip(if muted { ponzik_i18n::tr("Unmute") } else { ponzik_i18n::tr("Mute") }, "");
        }
        if tr != MASTER {
            let solo_r = Rect::new(mute.x + half + 4.0, mute.y, half, mute.h);
            let sid = wid("mx-solo", tr, 0);
            let shov = self.hovered(input, sid, solo_r);
            if shov && input.pressed {
                self.toggle_solo(tr);
            }
            let shv = self.hover_anim(sid, shov);
            let on = self.solo == Some(tr);
            let (fill, ink) = if on { (t.select, t.bg_deep) } else { (t.raised.mix(t.raised_hi, shv), t.text2) };
            c.fill_rrect(solo_r, 8.0, fill);
            let (sx, sy) = solo_r.center();
            draw_icon(c, Icon::Solo, sx, sy, 14.0, ink);
            if shov {
                self.set_tip(ponzik_i18n::tr("Solo"), "");
            }
        }
    }

    /// Corps d'une tranche VCA : la liste des pistes pilotées, le fader et le muet.
    fn vca_strip_body(&mut self, c: &mut Canvas, input: &Input, inner: Rect, tr: usize) {
        let t = self.theme;
        let members = self.project.vca_members(tr);
        let mut y = inner.y + 40.0;
        self.text(c, self.f.sans, 10.0, inner.x + 2.0, y + 6.0, ponzik_i18n::tr("Controls"), t.muted);
        y += 16.0;
        if members.is_empty() {
            let lines = [ponzik_i18n::tr("No tracks."), ponzik_i18n::tr("Right-click a"), ponzik_i18n::tr("track › VCA.")];
            for (k, line) in lines.iter().enumerate() {
                self.text(c, self.f.sans, 11.0, inner.x + 2.0, y + 8.0 + k as f32 * 15.0, line, t.text2.alpha(0.8));
            }
        }
        for (k, &m) in members.iter().take(9).enumerate() {
            let row = Rect::new(inner.x, y, inner.w, 20.0);
            let id = wid("mx-vca-row", tr, k);
            let hov = self.hovered(input, id, row);
            let hv = self.hover_anim(id, hov);
            c.fill_rrect(row, 6.0, t.raised.alpha(0.4 + 0.6 * hv));
            c.fill_circle(row.x + 9.0, row.y + 10.0, 3.0, self.track_tint(m));
            let name = self.project.tracks[m].name.clone();
            let label = if k == 8 && members.len() > 9 { tr_fmt!("+ {} more", members.len() - 8) } else { name };
            self.text_fit(c, self.f.sans, 11.0, row.x + 18.0, row.y + 10.0, &label, row.w - 22.0, t.text2);
            if hov {
                self.set_tip(ponzik_i18n::tr("Click: show · right-click: remove from VCA"), "");
                if input.pressed {
                    self.focus = Focus::Track(m);
                }
                if input.right_pressed {
                    self.project.tracks[m].vca = None;
                    self.commit(ponzik_i18n::tr("Remove from VCA"));
                }
            }
            y += 22.0;
        }

        let mute = Rect::new(inner.x, inner.bottom() - 26.0, inner.w, 26.0);
        let info_r = Rect::new(inner.x, mute.y - 28.0, inner.w, 24.0);
        let readout_y = info_r.y - 12.0;
        let top = inner.y + 38.0 + 22.0 * INSERT_SLOTS as f32 + 4.0 + 52.0 + 42.0;
        let fader = Rect::new(inner.x, top, inner.w, readout_y - 12.0 - top);
        if fader.h > 40.0 {
            self.track_fader(c, input, fader, tr);
        }
        let gain = self.project.tracks[tr].gain;
        self.text_center(c, self.f.mono, 12.0, inner.x + inner.w * 0.5, readout_y, &fmt_db(gain), t.text2);
        let n = members.len();
        let info = tr_fmt_n!("{} track", "{} tracks", n, n);
        let (cx, cy) = info_r.center();
        self.text_center(c, self.f.sans, 11.0, cx + 7.0, cy, &info, t.muted);
        let w = self.fonts.measure(self.f.sans, 11.0, &info);
        draw_icon(c, Icon::Vca, cx - w * 0.5 - 3.0, cy, 11.0, t.muted);

        let muted = self.project.tracks[tr].muted;
        let mid = wid("mx-mute", tr, 0);
        let hov = self.hovered(input, mid, mute);
        if hov && input.pressed {
            self.project.tracks[tr].muted = !muted;
            self.commit(ponzik_i18n::tr("Track mute"));
        }
        let hv = self.hover_anim(mid, hov);
        let (fill, ink) = if muted { (t.text.mix(WHITE, hv * 0.2), t.bg_deep) } else { (t.raised.mix(t.raised_hi, hv), t.text2) };
        c.fill_rrect(mute, 8.0, fill);
        let (mx, my) = mute.center();
        draw_icon(c, Icon::Mute, mx, my, 14.0, ink);
        if hov {
            self.set_tip(if muted { ponzik_i18n::tr("Unmute the VCA's tracks") } else { ponzik_i18n::tr("Mute all the VCA's tracks") }, "");
        }
    }

    /// Sélecteur de piste clé d'un insert : clé grise sans clé, de la couleur de la
    /// piste clé une fois branchée.
    #[allow(clippy::too_many_arguments)]
    fn key_selector(
        &mut self,
        c: &mut Canvas,
        input: &Input,
        id: u64,
        r: Rect,
        hov: bool,
        tr: usize,
        slot: usize,
        key: Option<usize>,
    ) {
        let t = self.theme;
        let hv = self.hover_anim(id, hov);
        let (cx, cy) = r.center();
        match key {
            Some(k) => {
                let col = self.track_tint(k);
                c.fill_rrect(r.inset(1.0), 5.0, col.alpha(0.16 + 0.14 * hv));
                draw_icon(c, Icon::Key, cx, cy, 12.0, col);
            }
            None => {
                if hv > 0.01 {
                    c.fill_rrect(r.inset(1.0), 5.0, t.raised_hi.alpha(hv));
                }
                draw_icon(c, Icon::Key, cx, cy, 12.0, t.muted.mix(t.text2, hv).alpha(0.55 + 0.45 * hv));
            }
        }
        if hov {
            let tip = match key {
                Some(k) => tr_fmt!("Key: {}\nClick: change · right-click: unplug", self.project.tracks[k].name),
                None => ponzik_i18n::tr("Sidechain input\nClick: choose the key track").to_string(),
            };
            self.set_tip(&tip, "");
            if input.pressed {
                self.open_popup(Popup::Sidechain { track: tr, slot }, r);
            }
            if input.right_pressed && key.is_some() {
                if let Some(fx) = self.project.tracks[tr].inserts[slot].as_mut() {
                    fx.sidechain = None;
                }
                self.commit(ponzik_i18n::tr("Unplug the key"));
            }
        }
    }

    /// Sonie du master : intégrée en chiffres, court terme en barre (−36 à 0 LUFS),
    /// le spectre en fond. Clic : la mesure repart de zéro.
    fn loudness_box(&mut self, c: &mut Canvas, input: &Input, r: Rect) {
        let t = self.theme;
        let id = wid("mx-lufs", 0, 0);
        let hov = self.hovered(input, id, r);
        let hv = self.hover_anim(id, hov);
        c.fill_rrect(r, 8.0, t.well.mix(t.raised, hv * 0.4));
        let prev = c.push_clip(r.inset(1.0));
        self.draw_spectrum(c, r, t.select.alpha(0.07), t.select.alpha(0.22));
        c.set_clip(Some(prev));
        let (integrated, short) = (self.loudness.integrated(), self.loudness.short_term());
        let fmt = |v: f32| if v.is_finite() { fmt_lufs(v) } else { "—".into() };
        let big = fmt(integrated);
        let x = self.text(c, self.f.mono_med, 15.0, r.x + 8.0, r.y + 15.0, &big, if integrated.is_finite() { t.text } else { t.muted });
        self.text(c, self.f.sans, 10.0, r.x + 8.0 + x + 4.0, r.y + 16.0, "LUFS", t.muted);
        // Court terme : une barre sous le chiffre, le repère à −14 LUFS (streaming).
        let bar = Rect::new(r.x + 8.0, r.bottom() - 13.0, r.w - 16.0, 4.0);
        c.fill_rrect(bar, 2.0, t.raised_hi);
        let pos = |v: f32| ((v + 36.0) / 36.0).clamp(0.0, 1.0);
        if short.is_finite() {
            let w = (bar.w * pos(short)).max(4.0);
            let col = if short > -6.0 { t.negative } else { t.accent };
            c.fill_rrect(Rect::new(bar.x, bar.y, w, bar.h), 2.0, col);
        }
        c.fill_rect(Rect::new((bar.x + bar.w * pos(-14.0)).round(), bar.y - 2.0, 1.0, bar.h + 4.0), t.text2.alpha(0.7));
        if hov {
            let tip = tr_fmt!("Master loudness\nIntegrated {} · 3 s {}\nClick: reset", fmt(integrated), fmt(short));
            self.set_tip(&tip, "");
        }
        if hov && input.pressed {
            self.loudness.reset();
        }
    }

    fn track_fader(&mut self, c: &mut Canvas, input: &Input, area: Rect, tr: usize) {
        let t = self.theme;
        const CAP_W: f32 = 34.0;
        const CAP_H: f32 = 18.0;
        let travel = (area.h - CAP_H).max(1.0);
        let y_of = |pos: f32| area.bottom() - CAP_H * 0.5 - pos * travel;
        let track_x = area.x + 42.0;
        let gain = self.project.tracks[tr].gain;
        let pos = gain_to_pos(gain);
        let cap = Rect::new(track_x + 2.0 - CAP_W * 0.5, y_of(pos) - CAP_H * 0.5, CAP_W, CAP_H);
        let zone = Rect::new(area.x + 24.0, area.y, 40.0, area.h);
        let id = wid("mx-fader", tr, 0);
        let hov = self.hovered(input, id, zone);
        if hov && input.pressed {
            if input.double || input.mods.ctrl {
                self.project.tracks[tr].gain = 1.0;
                self.commit(ponzik_i18n::tr("Track volume"));
            } else {
                let grab = if cap.contains(input.x, input.y) { input.y - y_of(pos) } else { 0.0 };
                self.begin_drag(id, input, grab);
            }
        }
        if let Some(d) = self.drag.filter(|d| d.id == id) {
            let p = ((area.bottom() - CAP_H * 0.5 - (input.y - d.v0)) / travel).clamp(0.0, 1.0);
            self.project.tracks[tr].gain = pos_to_gain(p);
            self.commit(ponzik_i18n::tr("Track volume"));
        }
        if hov && input.wheel != 0.0 {
            self.project.tracks[tr].gain = pos_to_gain((pos + input.wheel * 0.02).clamp(0.0, 1.0));
            self.commit(ponzik_i18n::tr("Track volume"));
        }
        let target = ponzik_engine::AutoTarget::TrackGain(tr);
        if hov && input.right_pressed {
            let popup = Popup::Param { target, value: gain, default: 1.0 };
            self.open_popup(popup, Rect::new(input.x, input.y, 0.0, 0.0));
        }

        for d in DB_TICKS {
            let y = y_of(gain_to_pos(10f32.powf(d / 20.0)));
            c.fill_rect(Rect::new(track_x - 10.0, y.round(), 5.0, 1.0), t.hairline.alpha(2.0));
            let label = fmt_tick(d);
            self.text_right(c, self.f.mono, 10.0, area.x + 22.0, y, &label, t.muted);
        }
        c.fill_rrect(Rect::new(track_x, area.y + CAP_H * 0.5, 4.0, travel), 2.0, t.well);

        let meter: Meter = if self.project.tracks[tr].kind == TrackKind::Vca {
            // Un VCA ne porte pas de son : il montre la crête de ses pistes.
            self.project.vca_members(tr).iter().fold(Meter::default(), |mut m, &x| {
                let o = self.track_meters[x];
                for side in 0..2 {
                    m.level[side] = m.level[side].max(o.level[side]);
                    m.hold[side] = m.hold[side].max(o.hold[side]);
                }
                m
            })
        } else {
            self.track_meters[tr]
        };
        let warn = y_of(gain_to_pos(10f32.powf(-6.0 / 20.0)));
        let bottom = y_of(0.0);
        for side in 0..2 {
            let x = track_x + 22.0 + side as f32 * 7.0;
            c.fill_rrect(Rect::new(x, area.y + CAP_H * 0.5, 5.0, travel), 2.0, t.well);
            let top = y_of(gain_to_pos(meter.level[side]));
            if bottom - top > 0.5 {
                let prev = c.push_clip(Rect::new(x, warn, 5.0, bottom - warn));
                c.fill_rrect(Rect::new(x, top, 5.0, bottom - top), 2.0, t.meter);
                c.set_clip(Some(prev));
                let prev = c.push_clip(Rect::new(x, area.y, 5.0, warn - area.y));
                c.fill_rrect(Rect::new(x, top, 5.0, bottom - top), 2.0, t.accent);
                c.set_clip(Some(prev));
            }
            let hold = meter.hold[side];
            if hold > 1e-4 {
                let col = if hold >= 0.999 { t.negative } else { t.text2 };
                c.fill_rect(Rect::new(x, y_of(gain_to_pos(hold)).round() - 1.0, 5.0, 2.0), col);
            }
        }
        let hv = self.hover_anim(id, (hov && cap.contains(input.x, input.y)) || self.is_dragging(id));
        c.fill_rrect_vgrad(cap, 6.0, t.text.mix(WHITE, hv * 0.5), t.text.mix(t.raised_hi, 0.25).mix(WHITE, hv * 0.3));
        c.fill_rect(Rect::new(cap.x + 6.0, cap.y + CAP_H * 0.5 - 1.0, CAP_W - 12.0, 2.0), t.bg_deep.alpha(0.55));
        self.ctl_mark(c, (cap.right() - 3.0, cap.y + 3.0), 4.0, ponzik_project::midi_map::ControlTarget::Param(target));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::app::View;
    use ponzik_window::{Key, Modifiers};

    fn app() -> App {
        let project = ponzik_project::demo_project();
        let none = |_: &str| None;
        let (_engine, handle) = project.build_engine(48_000.0, &none);
        let mut app = App::new(handle, project, 48_000);
        app.set_view(View::Mixer);
        app
    }

    /// Un clic en (x, y), avec ou sans Ctrl, puis une image bouton relâché.
    fn click(app: &mut App, c: &mut Canvas, x: f32, y: f32, ctrl: bool) {
        let mods = Modifiers { ctrl, ..Modifiers::default() };
        let mut input = Input { x, y, inside: true, mods, ..Input::default() };
        app.frame(c, &input, 1.0 / 60.0);
        (input.down, input.pressed) = (true, true);
        app.frame(c, &input, 1.0 / 60.0);
        (input.down, input.pressed, input.released) = (false, false, true);
        app.frame(c, &input, 1.0 / 60.0);
    }

    fn frames(app: &mut App, c: &mut Canvas, input: &Input, n: usize) {
        for _ in 0..n {
            app.frame(c, input, 1.0 / 60.0);
        }
    }

    #[test]
    fn ctrl_click_then_ctrl_g_groups_and_the_chevron_folds() {
        let mut app = app();
        let mut c = Canvas::new(1600, 960);
        let idle = Input::default();
        frames(&mut app, &mut c, &idle, 30);
        // Noms de Kick (piste 1) et Clap (piste 2), après le master.
        click(&mut app, &mut c, 464.0, 146.0, true);
        click(&mut app, &mut c, 584.0, 146.0, true);
        assert_eq!(app.mixer_sel, [1, 2]);
        let ctrl = Modifiers { ctrl: true, ..Modifiers::default() };
        let keys = Input { keys: vec![(Key::Char('g'), ctrl)], ..Input::default() };
        app.frame(&mut c, &keys, 1.0 / 60.0);
        let bus = TRACKS - 1;
        assert_eq!(app.project.tracks[bus].kind, TrackKind::Bus);
        assert_eq!(app.project.bus_members(bus), [1, 2]);
        assert!(app.mixer_sel.is_empty());
        assert_eq!(app.focused_track(), bus);
        // Le bus prend la place de Kick, ses pistes le suivent.
        let order: Vec<usize> = app.mixer_layout(false).iter().map(|s| s.tr).collect();
        assert_eq!(order[..4], [MASTER, bus, 1, 2]);
        frames(&mut app, &mut c, &idle, 40);
        // Chevron du bus, à gauche de son nom.
        click(&mut app, &mut c, 425.0, 138.0, false);
        assert!(app.project.tracks[bus].folded);
        assert!(app.mixer_layout(false).iter().filter(|s| s.hidden).map(|s| s.tr).eq([1, 2]));
        app.undo();
        assert!(!app.project.tracks[bus].folded);
        app.undo();
        assert_eq!(app.project.tracks[1].output, MASTER);
        assert_eq!(app.project.tracks[bus].kind, TrackKind::Normal);
    }

    #[test]
    fn solo_keeps_the_bus_chain_and_the_vcas() {
        let mut app = app();
        let outer = app.project.add_bus(&[1, 2]).unwrap();
        let inner = app.project.add_bus(&[1]).unwrap();
        let vca = app.project.add_vca(&[2]).unwrap();
        // Solo du bus extérieur : ses pistes, le bus imbriqué et le VCA de la piste 2.
        let keep = app.solo_keep(outer);
        for t in [MASTER, outer, inner, 1, 2, vca] {
            assert!(keep.contains(&t), "{t}");
        }
        assert!(!keep.contains(&3));
        // Solo d'une piste du bus imbriqué : la chaîne jusqu'au master.
        assert_eq!(app.solo_keep(1), [MASTER, 1, inner, outer]);
        // Solo d'un VCA : ses pistes et leur chemin.
        assert_eq!(app.solo_keep(vca), [MASTER, 2, vca, outer]);
    }

    #[test]
    fn vca_refuses_effects_and_dissolving_undoes() {
        let mut app = app();
        let vca = app.project.add_vca(&[1]).unwrap();
        app.commit(ponzik_i18n::tr("New VCA"));
        app.add_effect(vca, "reverb");
        assert!(app.project.tracks[vca].inserts.iter().all(Option::is_none));
        app.dissolve_track(vca);
        assert_eq!(app.project.tracks[1].vca, None);
        app.undo();
        assert_eq!(app.project.tracks[1].vca, Some(vca));
    }
}
