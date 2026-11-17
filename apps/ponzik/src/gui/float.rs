//! Interface modulable : fenêtres d'appareil flottantes et séparateurs glissables.
//!
//! On attrape l'en-tête d'un appareil dans le panneau du bas et on le tire vers
//! le haut : il devient une fenêtre posée au-dessus de l'interface. Plusieurs
//! fenêtres s'empilent en couches (un clic amène au premier plan) ; les reposer
//! sur le panneau du bas les réancre. Le navigateur et le panneau du bas se
//! redimensionnent en tirant leur bord.

use ponzik_ui::{Canvas, Color, Rect};
use ponzik_i18n::{tr, tr_fmt};
use ponzik_window::Cursor;

use super::Input;
use super::app::{App, Focus};
use super::devices::Device;
use super::theme::*;
use super::visuals::device_key;
use super::widgets::{Icon, draw_icon, wid};

/// Hauteur d'une fenêtre neuve : la carte du panneau, un peu plus d'air.
const FLOAT_H: f32 = 232.0;
const MIN_W: f32 = 220.0;
const MIN_H: f32 = 120.0;
/// Il faut monter de ça au-dessus du panneau pour détacher.
const TEAR: f32 = 18.0;
/// Coin de redimensionnement.
const RESIZE: f32 = 18.0;
/// Demi-épaisseur de la prise d'un séparateur.
const GRIP: f32 = 4.0;
const SIDEBAR_MIN: f32 = 190.0;
const SIDEBAR_MAX: f32 = 460.0;
const PANEL_MIN: f32 = 180.0;
const PANEL_MAX: f32 = 620.0;
/// Largeur de la carte qui reste dans le panneau quand l'appareil flotte.
pub const PLACEHOLDER_W: f32 = 112.0;

pub struct FloatWin {
    pub device: Device,
    /// Type de l'appareil au détachement : s'il change, la fenêtre se ferme.
    kind: String,
    pub rect: Rect,
    /// Rectangle de départ de l'animation d'ouverture (la carte du panneau).
    from: Option<Rect>,
    /// Placée dans la zone principale (sinon : à la prochaine image).
    placed: bool,
    id: usize,
}

#[derive(Clone, Copy)]
enum Gesture {
    /// En-tête saisi dans le panneau ; décalage du pointeur dans la carte.
    Tear { device: Device, card: Rect, dx: f32, dy: f32 },
    Move { id: usize, dx: f32, dy: f32 },
    Resize { id: usize, x0: f32, y0: f32, w0: f32, h0: f32 },
    Sidebar { x0: f32, w0: f32 },
    Panel { y0: f32, h0: f32 },
}

/// Disposition de l'interface : tailles des zones et fenêtres flottantes.
pub struct Layout {
    /// Fenêtres dans l'ordre d'empilement : la dernière est au premier plan.
    pub wins: Vec<FloatWin>,
    gesture: Option<Gesture>,
    next: usize,
    pub sidebar_w: f32,
    pub panel_h: f32,
    /// Tailles retenues pour cette image (bornées par la fenêtre), panneaux ouverts.
    pub sidebar_open: f32,
    pub panel_open: f32,
    /// Fenêtre qui a reçu le clic en cours : elle garde la souris jusqu'au relâchement.
    owner: Option<usize>,
    /// Fenêtre la plus haute sous la souris.
    hit: Option<usize>,
    /// Zone principale de l'image, où s'ouvrent les fenêtres.
    main: Rect,
    /// Zones de l'image : panneau du bas (où l'on réancre), bords glissables.
    panel: Rect,
    sidebar_edge: Option<Rect>,
    panel_edge: Option<Rect>,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            wins: Vec::new(),
            gesture: None,
            next: 0,
            sidebar_w: SIDEBAR,
            panel_h: PANEL,
            sidebar_open: SIDEBAR,
            panel_open: PANEL,
            owner: None,
            hit: None,
            main: Rect::default(),
            panel: Rect::default(),
            sidebar_edge: None,
            panel_edge: None,
        }
    }
}

impl Layout {
    pub fn floating(&self, d: Device) -> bool {
        self.wins.iter().any(|w| w.device == d)
    }

    /// Drops the move or tear in progress (the click meant something else).
    pub fn cancel_gesture(&mut self) {
        self.gesture = None;
    }

    fn index(&self, id: usize) -> Option<usize> {
        self.wins.iter().position(|w| w.id == id)
    }

    fn raise(&mut self, i: usize) {
        let w = self.wins.remove(i);
        self.wins.push(w);
    }

    /// Position d'ouverture : centrée en haut de la zone principale, en cascade.
    fn place(&mut self) {
        let main = self.main;
        if main.w <= 0.0 {
            return;
        }
        let mut k = 0.0;
        for win in &mut self.wins {
            if win.placed {
                continue;
            }
            let r = &mut win.rect;
            r.x = (main.x + (main.w - r.w) * 0.5 + k * 36.0).round();
            r.y = (main.y + 64.0 + k * 36.0).round();
            win.from.get_or_insert(r.scale(0.94));
            win.placed = true;
            k += 1.0;
        }
    }
}

/// La souris pour ce qui est caché : position et bouton tenu, sans clic ni molette.
pub fn masked(input: &Input) -> Input {
    Input {
        inside: false,
        pressed: false,
        right_pressed: false,
        middle_pressed: false,
        wheel: 0.0,
        double: false,
        ..input.clone()
    }
}

fn lerp_rect(a: Rect, b: Rect, k: f32) -> Rect {
    let l = |x: f32, y: f32| x + (y - x) * k;
    Rect::new(l(a.x, b.x), l(a.y, b.y), l(a.w, b.w), l(a.h, b.h))
}

impl App {
    fn device_alive(&self, d: Device, kind: &str) -> bool {
        match d {
            Device::Instrument(ch) => self.project.channels.get(ch).is_some_and(|c| c.instrument.kind == kind),
            Device::Effect { track, slot } => self
                .project
                .tracks
                .get(track)
                .and_then(|t| t.inserts.get(slot))
                .is_some_and(|e| e.as_ref().is_some_and(|e| e.kind == kind)),
            Device::RackUnit { .. } => self.device_kind(d).0 == kind,
        }
    }

    /// Détache un appareil dans une fenêtre (ou ramène la sienne au premier plan).
    /// `from` : la carte du panneau, d'où part l'animation.
    pub fn detach(&mut self, device: Device, from: Option<Rect>) {
        if let Some(i) = self.layout.wins.iter().position(|w| w.device == device) {
            self.layout.raise(i);
            return;
        }
        let (kind, _) = self.device_kind(device);
        let rect = Rect::new(0.0, 0.0, self.device_width(device), FLOAT_H);
        let id = self.layout.next;
        self.layout.next += 1;
        self.anim.set(wid("float-open", id, 0), 0.0);
        self.layout.wins.push(FloatWin { device, kind, rect, from, placed: false, id });
        self.layout.place();
    }

    /// Remet l'appareil dans le panneau du bas, en montrant ce qui le contient.
    pub fn dock(&mut self, device: Device) {
        self.layout.wins.retain(|w| w.device != device);
        self.panel = true;
        match device {
            Device::Instrument(ch) => self.focus = Focus::Channel(ch),
            Device::Effect { track, .. } | Device::RackUnit { track, .. } => {
                if self.focused_track() != track {
                    self.focus = Focus::Track(track);
                }
            }
        }
    }

    /// Prise de l'en-tête d'une carte : glisser détache (ou déplace la fenêtre),
    /// double-clic détache (ou réancre).
    pub(super) fn float_grip(&mut self, input: &Input, grip: Rect, card: Rect, device: Device, floating: bool) {
        let id = wid("float-grip", device_key(device), floating as usize);
        if !self.hovered(input, id, grip) {
            return;
        }
        self.set_cursor(Cursor::Grab);
        if floating {
            self.set_tip(tr("Drag: move\nDouble-click or drag onto the panel: dock"), "");
        } else {
            self.set_tip(tr("Drag upward: detach"), "");
        }
        if !input.pressed {
            return;
        }
        if input.double {
            self.layout.gesture = None;
            self.drag = None;
            if floating { self.dock(device) } else { self.detach(device, Some(card)) }
            return;
        }
        let (dx, dy) = (input.x - card.x, input.y - card.y);
        if floating {
            if let Some(w) = self.layout.wins.iter().find(|w| w.device == device) {
                let wid_ = w.id;
                self.layout.gesture = Some(Gesture::Move { id: wid_, dx, dy });
                self.begin_drag(wid("float-move", wid_, 0), input, 0.0);
            }
        } else {
            self.layout.gesture = Some(Gesture::Tear { device, card, dx, dy });
            self.begin_drag(id, input, 0.0);
        }
    }

    /// Carte laissée dans le panneau par un appareil qui flotte.
    pub(super) fn float_placeholder(&mut self, c: &mut Canvas, input: &Input, card: Rect, device: Device) {
        let t = self.theme;
        let id = wid("float-ph", device_key(device), 0);
        let hov = self.hovered(input, id, card);
        let hv = self.hover_anim(id, hov);
        c.fill_rrect(card, 14.0, t.panel.alpha(0.35 + hv * 0.4));
        c.stroke_rrect(card, 14.0, 1.0, t.hairline.alpha(1.4 + hv));
        let (cx, cy) = card.center();
        draw_icon(c, Icon::Detach, cx, cy - 12.0, 18.0, t.muted.mix(t.accent, hv));
        let (kind, _) = self.device_kind(device);
        let name = ponzik_engine::registry::effect_kind(&kind)
            .map(|k| tr(k.name))
            .or_else(|| ponzik_engine::registry::instrument_kind(&kind).map(|k| tr(k.name)))
            .map_or_else(|| super::app::display_kind(&kind), str::to_string);
        let name = self.fit(self.f.sans, 12.0, &name, card.w - 16.0);
        self.text_center(c, self.f.sans, 12.0, cx, cy + 14.0, &name, t.muted.mix(t.text2, hv));
        if hov {
            self.set_tip(tr("In a window\nClick: bring to front · double-click: dock"), "");
            self.set_cursor(Cursor::Hand);
            if input.pressed {
                if input.double {
                    self.dock(device);
                } else if let Some(i) = self.layout.wins.iter().position(|w| w.device == device) {
                    let wid_ = self.layout.wins[i].id;
                    self.layout.raise(i);
                    self.anim.set(wid("float-flash", wid_, 0), 1.0);
                }
            }
        }
    }

    /// Avant de dessiner : séparateurs, gestes en cours, fenêtre survolée.
    /// Renvoie (largeur du navigateur, hauteur du panneau, souris captée par une
    /// fenêtre ou un séparateur).
    pub(super) fn layout_begin(&mut self, input: &Input, w: f32, h: f32) -> (f32, f32, bool) {
        let panel_max = PANEL_MAX.min(h - TOP_BAR - 160.0).max(PANEL_MIN);
        let sidebar_max = SIDEBAR_MAX.min(w * 0.4).max(SIDEBAR_MIN);

        // Séparateurs tirés : la taille suit la souris sans retard.
        match self.layout.gesture {
            Some(Gesture::Sidebar { x0, w0 }) => {
                self.layout.sidebar_w = (w0 + input.x - x0).clamp(SIDEBAR_MIN, sidebar_max).round();
                self.anim.set(wid("sidebar", 0, 0), self.layout.sidebar_w);
            }
            Some(Gesture::Panel { y0, h0 }) => {
                self.layout.panel_h = (h0 + y0 - input.y).clamp(PANEL_MIN, panel_max).round();
                self.anim.set(wid("panel", 0, 0), self.layout.panel_h);
            }
            _ => {}
        }
        let sidebar_w = self.layout.sidebar_w.min(sidebar_max);
        let panel_h = self.layout.panel_h.min(panel_max);
        let sb = self.anim.to(wid("sidebar", 0, 0), if self.sidebar { sidebar_w } else { 0.0 }, T_LAYOUT).round();
        let pn = self.anim.to(wid("panel", 0, 0), if self.panel { panel_h } else { 0.0 }, T_LAYOUT).round();

        let l = &mut self.layout;
        (l.sidebar_open, l.panel_open) = (sidebar_w, panel_h);
        l.main = Rect::new(sb, TOP_BAR, w - sb, h - TOP_BAR - pn);
        // Panneau fermé : on réancre quand même en visant le bas de la fenêtre.
        l.panel = Rect::new(sb, h - pn.max(56.0), w - sb, pn.max(56.0));
        l.sidebar_edge = (self.sidebar && sb > 1.0).then(|| Rect::new(sb - GRIP, TOP_BAR, 2.0 * GRIP, h - TOP_BAR));
        l.panel_edge = (self.panel && pn > 1.0).then(|| Rect::new(sb + GRIP, h - pn - GRIP, w - sb - GRIP, 2.0 * GRIP));

        // Fenêtres : celles dont l'appareil a disparu se ferment ; les autres restent attrapables.
        let alive: Vec<bool> = self.layout.wins.iter().map(|w| self.device_alive(w.device, &w.kind)).collect();
        let l = &mut self.layout;
        let mut alive = alive.into_iter();
        l.wins.retain(|_| alive.next().unwrap_or(false));
        l.place();
        for win in &mut l.wins {
            let r = &mut win.rect;
            r.w = r.w.clamp(MIN_W, (w - 16.0).max(MIN_W));
            r.h = r.h.clamp(MIN_H, (h - TOP_BAR).max(MIN_H));
            r.x = r.x.clamp(80.0 - r.w, (w - 80.0).max(0.0)).round();
            r.y = r.y.clamp(0.0, (h - 40.0).max(0.0)).round();
        }
        if !input.down && !input.released {
            l.owner = None;
        }
        let inside = input.inside;
        l.hit = l.wins.iter().rev().find(|w| inside && w.rect.contains(input.x, input.y)).map(|w| w.id);
        if input.pressed && let Some(id) = l.hit {
            l.owner = Some(id);
            let i = l.index(id).expect("window under the mouse");
            l.raise(i);
        }

        // Prise d'un séparateur.
        let free = self.drag.is_none() && self.layout.hit.is_none() && input.inside;
        let on_sidebar = free && self.layout.sidebar_edge.is_some_and(|r| r.contains(input.x, input.y));
        let on_panel = free && !on_sidebar && self.layout.panel_edge.is_some_and(|r| r.contains(input.x, input.y));
        if input.pressed && (on_sidebar || on_panel) {
            if input.double {
                // Double-clic : taille par défaut, en douceur.
                if on_sidebar { self.layout.sidebar_w = SIDEBAR } else { self.layout.panel_h = PANEL }
            } else if on_sidebar {
                self.layout.gesture = Some(Gesture::Sidebar { x0: input.x, w0: sidebar_w });
                self.begin_drag(wid("split", 0, 0), input, 0.0);
            } else {
                self.layout.gesture = Some(Gesture::Panel { y0: input.y, h0: panel_h });
                self.begin_drag(wid("split", 1, 0), input, 0.0);
            }
        }
        let busy = self.layout.gesture.is_some() || self.layout.owner.is_some();
        let covered = busy || (self.drag.is_none() && (self.layout.hit.is_some() || on_sidebar || on_panel));
        (sb, pn, covered)
    }

    /// Après l'interface : poignées des séparateurs, puis les fenêtres de bas en haut.
    pub(super) fn float_layer(&mut self, c: &mut Canvas, input: &Input) {
        let t = self.theme;
        self.splitters(c, input);

        // Arrachement : la carte quitte le panneau par le haut et devient une fenêtre.
        if let Some(Gesture::Tear { device, card, dx, dy }) = self.layout.gesture {
            let lift = ((card.y + dy - input.y) / (card.y + dy - self.layout.panel.y + TEAR).max(1.0)).clamp(0.0, 1.0);
            if !input.down {
                self.layout.gesture = None;
            } else if input.y < self.layout.panel.y - TEAR {
                self.detach(device, None);
                if let Some(win) = self.layout.wins.iter_mut().find(|w| w.device == device) {
                    win.rect.x = input.x - dx;
                    win.rect.y = input.y - dy;
                    win.from = Some(win.rect.scale(0.97));
                    win.placed = true;
                    let id = win.id;
                    self.layout.gesture = Some(Gesture::Move { id, dx, dy });
                    self.layout.owner = Some(id);
                    self.begin_drag(wid("float-move", id, 0), input, 0.0);
                }
            } else if lift > 0.02 {
                // Fantôme de la carte qui suit la souris.
                let ghost = Rect::new(input.x - dx, input.y - dy, card.w, card.h.min(FLOAT_H));
                c.fill_rrect(ghost, 14.0, t.accent.alpha(0.05 * lift));
                c.stroke_rrect(ghost, 14.0, 1.5, t.accent.alpha(0.7 * lift));
                draw_icon(c, Icon::Detach, ghost.x + ghost.w * 0.5, ghost.y + ghost.h * 0.5, 22.0, t.accent.alpha(lift));
            }
        }

        if input.down && matches!(self.layout.gesture, Some(Gesture::Tear { .. } | Gesture::Move { .. })) {
            self.set_cursor(Cursor::Grab);
        }

        // Déplacement et redimensionnement.
        let mut docking = false;
        match self.layout.gesture {
            Some(Gesture::Move { id, dx, dy }) => {
                let over_panel = input.inside && self.layout.panel.contains(input.x, input.y);
                if let Some(i) = self.layout.index(id) {
                    if !input.down {
                        self.layout.gesture = None;
                        if over_panel {
                            let device = self.layout.wins[i].device;
                            self.dock(device);
                        }
                    } else {
                        let r = &mut self.layout.wins[i].rect;
                        (r.x, r.y) = ((input.x - dx).round(), (input.y - dy).round());
                        docking = over_panel;
                    }
                } else {
                    self.layout.gesture = None;
                }
            }
            Some(Gesture::Resize { id, x0, y0, w0, h0 }) => {
                if let Some(i) = self.layout.index(id).filter(|_| input.down) {
                    let r = &mut self.layout.wins[i].rect;
                    r.w = (w0 + input.x - x0).max(MIN_W).round();
                    r.h = (h0 + input.y - y0).max(MIN_H).round();
                } else {
                    self.layout.gesture = None;
                }
            }
            Some(Gesture::Sidebar { .. } | Gesture::Panel { .. }) if !input.down => self.layout.gesture = None,
            _ => {}
        }

        // Le panneau du bas s'allume quand on y lâcherait la fenêtre.
        let dv = self.anim.to(wid("float-dock", 0, 0), docking as u8 as f32, T_HOVER);
        if dv > 0.01 {
            let p = self.layout.panel.inset(8.0);
            c.fill_rrect(p, 16.0, t.accent.alpha(0.07 * dv));
            c.stroke_rrect(p, 16.0, 1.5, t.accent.alpha(0.8 * dv));
            let (cx, cy) = p.center();
            c.fill_circle(cx, cy, 22.0, t.bg_deep.alpha(0.9 * dv));
            draw_icon(c, Icon::Dock, cx, cy, 20.0, t.accent.alpha(dv));
        }

        let top = self.layout.wins.last().map(|w| w.id);
        for i in 0..self.layout.wins.len() {
            let Some(win) = self.layout.wins.get(i) else { break };
            let (id, device, rect, from) = (win.id, win.device, win.rect, win.from);
            let open = self.anim.to(wid("float-open", id, 0), 1.0, T_LAYOUT);
            let r = lerp_rect(from.unwrap_or(rect), rect, open);
            let live = match self.layout.owner {
                Some(o) => o == id,
                None => self.layout.hit == Some(id),
            };
            let mut inp = if live { input.clone() } else { masked(input) };
            let moving = matches!(self.layout.gesture, Some(Gesture::Move { id: m, .. }) if m == id);

            // Ombre portée : plus large pendant le déplacement, la fenêtre semble levée.
            let lift = self.anim.to(wid("float-lift", id, 0), moving as u8 as f32, T_HOVER);
            let depth = (8.0 + lift * 8.0) * open;
            c.fill_rrect(r.translate(0.0, depth).inset(-depth * 0.6), 20.0, Color::rgba(0, 0, 0, (50.0 * open) as u8));
            c.fill_rrect(r.translate(0.0, 3.0).inset(-1.5), 16.0, Color::rgba(0, 0, 0, (90.0 * open) as u8));

            // Coin de redimensionnement, servi avant le contenu.
            let corner = Rect::new(r.right() - RESIZE, r.bottom() - RESIZE, RESIZE, RESIZE);
            let rid = wid("float-resize", id, 0);
            let rhov = self.hovered(&inp, rid, corner);
            if rhov {
                self.set_tip(tr("Drag: resize"), "");
                if inp.pressed {
                    self.layout.gesture = Some(Gesture::Resize { id, x0: inp.x, y0: inp.y, w0: rect.w, h0: rect.h });
                    self.begin_drag(rid, &inp, 0.0);
                    inp = masked(&inp);
                }
            }

            let prev = c.push_clip(r.inset(-1.0));
            match self.device_lock(device) {
                // Frozen: read-only under frost, with its Unfreeze button.
                Some(target) => {
                    self.device_card(c, &masked(&inp), r, device, true);
                    self.frost_card(c, &inp, r, target, true);
                }
                None => self.device_card(c, &inp, r, device, true),
            }
            let front = top == Some(id);
            let flash = self.anim.to(wid("float-flash", id, 0), 0.0, 0.2);
            c.stroke_rrect(r, 14.0, 1.0, t.hairline.alpha(if front { 2.6 } else { 1.2 }));
            if flash > 0.01 {
                c.stroke_rrect(r, 14.0, 1.5, t.accent.alpha(flash));
            }
            if rhov || self.is_dragging(rid) {
                self.set_cursor(Cursor::ResizeNwSe);
            }
            let rv = self.hover_anim(rid, rhov || self.is_dragging(rid));
            let col = t.muted.alpha(0.6).mix(t.accent, rv);
            for k in 0..3 {
                let o = 4.0 + k as f32 * 4.0;
                c.stroke_polyline(&[(r.right() - 4.0, r.bottom() - o), (r.right() - o, r.bottom() - 4.0)], 1.2, col);
            }
            if open < 0.999 {
                c.fill_rrect(r, 14.0, t.bg.alpha(1.0 - open));
            }
            c.set_clip(Some(prev));
        }
    }

    fn splitters(&mut self, c: &mut Canvas, input: &Input) {
        let t = self.theme;
        let edges = [(self.layout.sidebar_edge, 0usize), (self.layout.panel_edge, 1)];
        let free = self.layout.hit.is_none() && input.inside;
        for (edge, k) in edges {
            let Some(e) = edge else { continue };
            let id = wid("split", k, 0);
            let corner = k == 1 && self.layout.sidebar_edge.is_some_and(|s| s.contains(input.x, input.y));
            let hov = free && !corner && self.hovered(input, id, e);
            let hv = self.hover_anim(id, hov || self.is_dragging(id));
            if hov || self.is_dragging(id) {
                self.set_cursor(if k == 0 { Cursor::ResizeH } else { Cursor::ResizeV });
            }
            if hov {
                let tip = if k == 0 { tr("Drag: browser width") } else { tr("Drag: panel height") };
                self.set_tip(&tr_fmt!("{}\nDouble-click: default size", tip), "");
            }
            if hv < 0.01 {
                continue;
            }
            let (cx, cy) = e.center();
            if k == 0 {
                c.fill_rect(Rect::new(cx - 1.0, e.y, 2.0, e.h), t.accent.alpha(0.35 * hv));
                c.fill_rrect(Rect::new(cx - 2.5, cy - 18.0, 5.0, 36.0), 2.5, t.accent.alpha(hv));
            } else {
                c.fill_rect(Rect::new(e.x, cy - 1.0, e.w, 2.0), t.accent.alpha(0.35 * hv));
                c.fill_rrect(Rect::new(cx - 18.0, cy - 2.5, 36.0, 5.0), 2.5, t.accent.alpha(hv));
            }
        }
    }

    /// Captures : détache les `n` premiers appareils du panneau.
    pub fn screenshot_floats(&mut self, n: usize) {
        for d in self.panel_devices().into_iter().take(n) {
            self.detach(d, None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        let project = ponzik_project::demo_project();
        let none = |_: &str| None;
        let (_engine, handle) = project.build_engine(48_000.0, &none);
        App::new(handle, project, 48_000)
    }

    /// Une image avec la souris en (x, y) ; `down` : bouton gauche tenu.
    fn step(app: &mut App, c: &mut Canvas, input: &mut Input, x: f32, y: f32, down: bool) {
        input.pressed = down && !input.down;
        input.released = !down && input.down;
        (input.x, input.y, input.inside, input.down) = (x, y, true, down);
        app.frame(c, input, 1.0 / 60.0);
    }

    #[test]
    fn tear_off_move_and_dock() {
        let mut app = app();
        let mut c = Canvas::new(1600, 960);
        let mut i = Input::default();
        // En-tête de l'instrument (après la carte des effets de notes), tiré vers le haut.
        step(&mut app, &mut c, &mut i, 450.0, 750.0, false);
        step(&mut app, &mut c, &mut i, 450.0, 750.0, true);
        step(&mut app, &mut c, &mut i, 600.0, 400.0, true);
        step(&mut app, &mut c, &mut i, 640.0, 420.0, true);
        step(&mut app, &mut c, &mut i, 640.0, 420.0, false);
        assert_eq!(app.layout.wins.len(), 1);
        let r = app.layout.wins[0].rect;
        // La fenêtre garde la prise de l'en-tête : saisi 100 px après le bord de la carte.
        assert_eq!((r.x, r.y), (640.0 - 100.0, 420.0 - 15.0));
        // Reposée sur le panneau du bas : réancrée.
        step(&mut app, &mut c, &mut i, 640.0, 420.0, true);
        step(&mut app, &mut c, &mut i, 900.0, 820.0, true);
        step(&mut app, &mut c, &mut i, 900.0, 820.0, false);
        assert!(app.layout.wins.is_empty());
    }

    #[test]
    fn splitters_and_grips_set_the_cursor() {
        let mut app = app();
        let mut c = Canvas::new(1600, 960);
        let mut i = Input::default();
        step(&mut app, &mut c, &mut i, 10.0, 10.0, false);
        let (sx, sy) = app.layout.sidebar_edge.expect("browser open").center();
        step(&mut app, &mut c, &mut i, sx, sy, false);
        assert_eq!(app.cursor, Cursor::ResizeH);
        // Pendant le glisser, la forme tient même si la souris quitte le bord.
        step(&mut app, &mut c, &mut i, sx, sy, true);
        step(&mut app, &mut c, &mut i, sx + 40.0, sy, true);
        assert_eq!(app.cursor, Cursor::ResizeH);
        step(&mut app, &mut c, &mut i, sx + 40.0, sy, false);
        let (px, py) = app.layout.panel_edge.expect("panel open").center();
        step(&mut app, &mut c, &mut i, px, py, false);
        assert_eq!(app.cursor, Cursor::ResizeV);
        // En-tête de l'instrument dans le panneau : une prise à saisir.
        step(&mut app, &mut c, &mut i, 450.0, 750.0, false);
        assert_eq!(app.cursor, Cursor::Grab);
    }

    #[test]
    fn windows_stack_and_splitters_resize() {
        let mut app = app();
        let mut c = Canvas::new(1600, 960);
        let mut i = Input::default();
        app.focus = Focus::Track(0);
        app.screenshot_floats(2);
        step(&mut app, &mut c, &mut i, 10.0, 10.0, false);
        let (a, b) = (app.layout.wins[0].device, app.layout.wins[1].device);
        // Un clic sur la fenêtre du dessous, là où elle dépasse, l'amène devant.
        let r = app.layout.wins[0].rect;
        step(&mut app, &mut c, &mut i, r.x + 8.0, r.y + 20.0, false);
        step(&mut app, &mut c, &mut i, r.x + 8.0, r.y + 20.0, true);
        step(&mut app, &mut c, &mut i, r.x + 8.0, r.y + 20.0, false);
        assert_eq!(app.layout.wins.iter().map(|w| w.device).collect::<Vec<_>>(), vec![b, a]);

        // Bord du navigateur : tiré, puis borné.
        step(&mut app, &mut c, &mut i, SIDEBAR, 600.0, false);
        step(&mut app, &mut c, &mut i, SIDEBAR, 600.0, true);
        step(&mut app, &mut c, &mut i, SIDEBAR + 70.0, 600.0, true);
        step(&mut app, &mut c, &mut i, SIDEBAR + 70.0, 600.0, false);
        assert_eq!(app.layout.sidebar_w, SIDEBAR + 70.0);
        step(&mut app, &mut c, &mut i, SIDEBAR + 70.0, 600.0, true);
        step(&mut app, &mut c, &mut i, 40.0, 600.0, true);
        step(&mut app, &mut c, &mut i, 40.0, 600.0, false);
        assert_eq!(app.layout.sidebar_w, SIDEBAR_MIN);
    }
}
