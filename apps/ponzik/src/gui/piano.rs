//! Piano roll : notes du canal sélectionné dans le pattern courant.

use ponzik_engine::{Note, PlayMode, TICKS_PER_BAR, TICKS_PER_STEP};
use ponzik_i18n::{tr, tr_fmt, tr_fmt_n, tr_noop};
use ponzik_project::{ChordKind, ScaleKind, Voicing, revoice};
use ponzik_ui::{Canvas, Color, Rect};
use ponzik_window::{Cursor, Key};

use super::Input;
use super::app::{App, Focus, Popup, Rename, View};
use super::rack::note_name;
use super::theme::*;
use super::widgets::{Icon, draw_icon, wid};

const KEY_H: f32 = 16.0;
const KEYBOARD: f32 = 64.0;
const RULER: f32 = 28.0;
const VELOCITY: f32 = 76.0;
/// Lane of chances, conditions or repeats, under the velocities.
const LANE: f32 = 58.0;
const EDGE: f32 = 7.0;
const SNAPS: [u32; 4] = [96, 48, 24, 12];
const PIANO_HELP: &str = tr_noop("Piano roll
Click: place · edge: length · right-click: erase
Ctrl + drag: select · ruler: loop
Ctrl+B: duplicate · Ctrl+Q: quantize
Wheel: scroll · Shift: horizontal · Ctrl: zoom
Middle button: pan the view");

#[derive(Clone, Debug)]
pub enum PianoDrag {
    /// Déplacement : notes d'origine (indice, note), point de départ en ticks et en note.
    Move { orig: Vec<(usize, Note)>, tick0: f32, key0: f32 },
    Resize { orig: Vec<(usize, Note)>, tick0: f32 },
    Select { x0: f32, y0: f32 },
    Velocity,
}

fn is_black(key: u8) -> bool {
    matches!(key % 12, 1 | 3 | 6 | 8 | 10)
}

/// Forces d'humanisation : nom, écart de début (ticks, ±), écart de vélocité (±, relatif).
pub const HUMANIZE: [(&str, u32, f32); 3] = [(tr_noop("Light"), 3, 0.05), (tr_noop("Medium"), 6, 0.1), (tr_noop("Strong"), 12, 0.2)];

pub fn chord_name(kind: ChordKind) -> &'static str {
    match kind {
        ChordKind::Major => tr_noop("Major"),
        ChordKind::Minor => tr_noop("Minor"),
        ChordKind::Diminished => tr_noop("Diminished"),
        ChordKind::Augmented => tr_noop("Augmented"),
        ChordKind::Sus2 => tr_noop("Sus 2"),
        ChordKind::Sus4 => tr_noop("Sus 4"),
        ChordKind::Power => tr_noop("Power"),
        ChordKind::Major7 => tr_noop("Major 7"),
        ChordKind::Minor7 => tr_noop("Minor 7"),
        ChordKind::Dominant7 => tr_noop("Dominant 7"),
        ChordKind::HalfDiminished7 => tr_noop("Half-diminished"),
        ChordKind::Diminished7 => tr_noop("Diminished 7"),
        ChordKind::MinorMajor7 => tr_noop("Minor maj. 7"),
        ChordKind::Add9 => tr_noop("Add 9"),
        ChordKind::MinorAdd9 => tr_noop("Minor add 9"),
        ChordKind::Six => tr_noop("Sixth"),
        ChordKind::Minor6 => tr_noop("Minor 6"),
        ChordKind::SixNine => tr_noop("Six-nine"),
        ChordKind::SevenSus4 => tr_noop("7 sus 4"),
        ChordKind::Major9 => tr_noop("Major 9"),
        ChordKind::Minor9 => tr_noop("Minor 9"),
        ChordKind::Dominant9 => tr_noop("Dominant 9"),
        ChordKind::Add11 => tr_noop("Add 11"),
        ChordKind::Minor11 => tr_noop("Minor 11"),
        ChordKind::Dominant11 => tr_noop("Dominant 11"),
        ChordKind::Major13 => tr_noop("Major 13"),
        ChordKind::Dominant13 => tr_noop("Dominant 13"),
    }
}

/// Forme d'accord quand une gamme est fixée : la qualité vient de la gamme,
/// seuls le nombre de notes et leur empilement comptent.
pub const SCALE_SHAPES: [(Option<ChordKind>, &str, &str); 11] = [
    (None, tr_noop("Triad"), "3"),
    (Some(ChordKind::Six), tr_noop("Sixth"), "6"),
    (Some(ChordKind::Major7), tr_noop("Seventh"), "7"),
    (Some(ChordKind::Major9), tr_noop("Ninth"), "9"),
    (Some(ChordKind::Minor11), tr_noop("Eleventh"), "11"),
    (Some(ChordKind::Major13), tr_noop("Thirteenth"), "13"),
    (Some(ChordKind::Add9), tr_noop("Add 9"), "add9"),
    (Some(ChordKind::Sus2), tr_noop("Sus 2"), "sus2"),
    (Some(ChordKind::Sus4), tr_noop("Sus 4"), "sus4"),
    (Some(ChordKind::SevenSus4), tr_noop("7 sus 4"), "7sus4"),
    (Some(ChordKind::Power), tr_noop("Power"), "5"),
];

/// Forme (nom, symbole) d'un type d'accord une fois ramené dans la gamme.
pub fn scale_shape(kind: Option<ChordKind>) -> (&'static str, &'static str) {
    use ChordKind as K;
    let i = SCALE_SHAPES.iter().position(|s| s.0 == kind).unwrap_or(match kind {
        None | Some(K::Major | K::Minor | K::Diminished | K::Augmented) => 0,
        Some(K::Minor6) => 1,
        Some(K::MinorAdd9) => 6,
        Some(K::SixNine | K::Minor9 | K::Dominant9) => 3,
        Some(K::Add11 | K::Dominant11) => 4,
        Some(K::Dominant13) => 5,
        Some(_) => 2,
    });
    (SCALE_SHAPES[i].1, SCALE_SHAPES[i].2)
}

/// Case du choix d'accord : type (`None` : triade de la gamme), libellé, nom complet.
type ChordChoice = (Option<ChordKind>, &'static str, &'static str);

/// Identifiant de l'animation d'une note posée (hauteur, début).
fn pop_id(key: u8, start: u32) -> u64 {
    wid("pr-pop", usize::from(key), start as usize)
}

/// Libellés des renversements.
const INVERSIONS: [&str; 4] = [tr_noop("Root"), tr_noop("1st"), tr_noop("2nd"), tr_noop("3rd")];

/// Symbole court ; « M » pour l'accord majeur, « b5 » à la place de « ♭5 »
/// (les polices embarquées n'ont pas ♭).
pub fn chord_symbol(kind: ChordKind) -> &'static str {
    match kind {
        ChordKind::Major => "M",
        ChordKind::HalfDiminished7 => "m7b5",
        k => k.symbol(),
    }
}

impl App {
    pub(super) fn piano_channel(&self) -> Option<usize> {
        self.selected_channel().or_else(|| (!self.project.channels.is_empty()).then_some(0))
    }

    pub(super) fn notes_mut(&mut self, ch: usize) -> &mut Vec<Note> {
        let n = self.project.channels.len();
        let pat = self.project.current_pattern;
        let notes = &mut self.project.patterns[pat].notes;
        if notes.len() < n {
            notes.resize(n, Vec::new());
        }
        &mut notes[ch]
    }

    /// Notes que le tampon d'accord pose sur `root` (au moins `root`).
    fn chord_at(&self, root: u8) -> Vec<u8> {
        let kind = self.piano.chord.unwrap_or(ChordKind::Major);
        let notes = self.project.scale.shaped_chord(self.project.key, root, kind);
        let notes = revoice(&notes, usize::from(self.piano.chord_inv), self.piano.voicing);
        if notes.is_empty() { vec![root] } else { notes }
    }

    /// Hauteurs posées par un clic sur `key` : collée à la gamme si demandé, en accord si le tampon est actif.
    fn stamp_keys(&self, key: u8) -> Vec<u8> {
        let root = if self.piano.scale_snap { self.project.snap(key) } else { key };
        if self.piano.chord_stamp { self.chord_at(root) } else { vec![root] }
    }

    /// Choix de l'accord : formes (ou types, sans gamme), renversement,
    /// disposition, et l'accord sur la tonique en aperçu. Renvoie faux quand il se ferme.
    pub(super) fn chord_popup(&mut self, c: &mut Canvas, input: &Input, anchor: Rect, screen: Rect) -> bool {
        let t = self.theme;
        let open = self.anim.to(wid("popup", 0, 0), 1.0, T_HOVER);
        const GAP: f32 = 4.0;
        const COLS: usize = 4;
        const HEAD: f32 = 24.0;
        let (w, cell_h) = (372.0, 34.0);
        let cell_w = (w - 24.0 - (COLS - 1) as f32 * GAP) / COLS as f32;
        let in_scale = self.project.scale != ScaleKind::Chromatic;

        // Sections : (titre, [(choix, libellé, nom complet)]).
        let mut forms: Vec<(String, Vec<ChordChoice>)> = Vec::new();
        if in_scale {
            let title = tr_fmt!("Shape · {} {}", super::app::key_name(self.project.key), tr(self.project.scale.name()).to_lowercase());
            forms.push((title, SCALE_SHAPES.iter().map(|&(k, name, _)| (k, name, name)).collect()));
        } else {
            // Familles dans l'ordre de ChordKind::ALL (sans dépendre de leurs libellés).
            let mut groups: Vec<&'static str> = Vec::new();
            for k in ChordKind::ALL {
                if !groups.contains(&k.group()) {
                    groups.push(k.group());
                }
            }
            for group in groups {
                let items = ChordKind::ALL
                    .into_iter()
                    .filter(|k| k.group() == group)
                    .map(|k| (Some(k), chord_symbol(k), chord_name(k)))
                    .collect();
                forms.push((tr(group).to_string(), items));
            }
        }
        let rows = |n: usize| n.div_ceil(COLS) as f32 * (cell_h + GAP);
        let forms_h: f32 = forms.iter().map(|(_, v)| HEAD + rows(v.len())).sum();
        let preview_h = 54.0;
        let h = 40.0 + forms_h + 2.0 * (HEAD + cell_h + GAP) + preview_h + 14.0;
        let x = anchor.x.min(screen.right() - 12.0 - w).max(12.0);
        let mut y0 = anchor.bottom() + 8.0;
        if y0 + h > screen.bottom() - 8.0 {
            y0 = (screen.bottom() - 8.0 - h).max(8.0);
        }
        let r = Rect::new(x, y0 + (1.0 - open) * 6.0, w, h);
        c.fill_rrect(r.translate(0.0, 6.0).inset(-2.0), 16.0, Color::rgba(0, 0, 0, (90.0 * open) as u8));
        c.fill_rrect(r, 14.0, t.panel);
        c.stroke_rrect(r, 14.0, 1.0, t.hairline.alpha(1.6));
        self.text(c, self.f.sans_med, 12.0, r.x + 16.0, r.y + 20.0, tr("Chord"), t.muted);

        let over = |b: Rect| input.inside && b.contains(input.x, input.y);
        let mut hovered_name: Option<&str> = None;
        let mut y = r.y + 34.0;
        let mut n = 0;
        let cur_shape = scale_shape(self.piano.chord).0;
        for (title, items) in &forms {
            self.text(c, self.f.sans_med, 11.0, r.x + 16.0, y + HEAD * 0.5, title, t.muted);
            y += HEAD;
            for (j, &(kind, label, name)) in items.iter().enumerate() {
                let cell = Rect::new(r.x + 12.0 + (j % COLS) as f32 * (cell_w + GAP), y + (j / COLS) as f32 * (cell_h + GAP), cell_w, cell_h);
                let cur = if in_scale { name == cur_shape } else { self.piano.chord.unwrap_or(ChordKind::Major) == kind.unwrap_or(ChordKind::Major) };
                let hov = over(cell);
                if hov {
                    hovered_name = Some(name);
                }
                self.choice_cell(c, cell, wid("chord-kind", n, 0), hov, cur, tr(label));
                n += 1;
                if hov && input.pressed {
                    self.piano.chord = kind;
                    self.piano.chord_stamp = true;
                }
            }
            y += rows(items.len());
        }

        // Renversement et disposition.
        self.text(c, self.f.sans_med, 11.0, r.x + 16.0, y + HEAD * 0.5, tr("Inversion"), t.muted);
        y += HEAD;
        for (i, label) in INVERSIONS.iter().enumerate() {
            let cell = Rect::new(r.x + 12.0 + i as f32 * (cell_w + GAP), y, cell_w, cell_h);
            let hov = over(cell);
            self.choice_cell(c, cell, wid("chord-inv", i, 0), hov, usize::from(self.piano.chord_inv) == i, tr(label));
            if hov && input.pressed {
                self.piano.chord_inv = i as u8;
                self.piano.chord_stamp = true;
            }
        }
        y += cell_h + GAP;
        self.text(c, self.f.sans_med, 11.0, r.x + 16.0, y + HEAD * 0.5, tr("Voicing"), t.muted);
        y += HEAD;
        let vw = (w - 24.0 - 2.0 * GAP) / 3.0;
        for (i, v) in Voicing::ALL.into_iter().enumerate() {
            let cell = Rect::new(r.x + 12.0 + i as f32 * (vw + GAP), y, vw, cell_h);
            let hov = over(cell);
            self.choice_cell(c, cell, wid("chord-voicing", i, 0), hov, self.piano.voicing == v, tr(v.name()));
            if hov && input.pressed {
                self.piano.voicing = v;
                self.piano.chord_stamp = true;
            }
        }
        y += cell_h + GAP + 6.0;

        // Aperçu : l'accord posé sur la tonique, sur deux octaves et demie de clavier.
        let kb = Rect::new(r.x + 12.0, y, w - 24.0, preview_h - 8.0);
        let root = 60 + self.project.key % 12;
        let notes = self.chord_at(root);
        let low = notes.first().map_or(48, |&n| (n / 12) * 12).min(root / 12 * 12);
        let whites: Vec<u8> = (low..low + 36).filter(|k| !matches!(k % 12, 1 | 3 | 6 | 8 | 10)).collect();
        let kw = kb.w / whites.len() as f32;
        c.fill_rrect(kb, 6.0, t.well);
        for (i, &k) in whites.iter().enumerate() {
            let key = Rect::new(kb.x + i as f32 * kw + 0.5, kb.y + 1.0, kw - 1.0, kb.h - 2.0);
            let on = notes.contains(&k);
            c.fill_rrect(key, 3.0, if on { t.accent } else { t.text2.alpha(0.18) });
        }
        for (i, &k) in whites.iter().enumerate() {
            let black = k + 1;
            if matches!(black % 12, 1 | 3 | 6 | 8 | 10) && black < low + 36 {
                let key = Rect::new(kb.x + (i as f32 + 1.0) * kw - kw * 0.3, kb.y + 1.0, kw * 0.6, kb.h * 0.58);
                let on = notes.contains(&black);
                c.fill_rrect(key, 2.0, if on { t.accent.mix(WHITE, 0.15) } else { t.bg_deep });
            }
        }
        // Nom de l'accord obtenu (ou du choix survolé), en haut à droite.
        let shown = match hovered_name {
            Some(name) => tr(name).to_string(),
            None => {
                ChordKind::identify(&notes).map_or_else(
                    || tr_fmt_n!("{} note", "{} notes", notes.len(), notes.len()),
                    |k| format!("{}{}", super::app::key_name(notes[0]), chord_symbol(k)),
                )
            }
        };
        self.text_right(c, self.f.mono_med, 12.0, r.right() - 16.0, r.y + 20.0, &shown, t.text2);

        let clicked = input.pressed || input.right_pressed;
        !clicked || r.contains(input.x, input.y) || anchor.contains(input.x, input.y)
    }

    /// Case de choix d'un popup : surlignée au survol, pleine d'accent si choisie.
    fn choice_cell(&mut self, c: &mut Canvas, cell: Rect, id: u64, hov: bool, cur: bool, label: &str) {
        let t = self.theme;
        let hv = self.hover_anim(id, hov);
        let on = self.anim.to(id ^ 0x6f6e, cur as u8 as f32, T_HOVER);
        c.fill_rrect(cell, 8.0, t.raised.mix(t.raised_hi, hv).alpha(0.55 + 0.45 * hv).mix(t.accent, on));
        let (cx, cy) = cell.center();
        let ink = t.text2.mix(t.text, hv).mix(t.on_accent, on);
        let label = self.fit(self.f.sans_med, 12.0, label, cell.w - 8.0);
        self.text_center(c, self.f.sans_med, 12.0, cx, cy, &label, ink);
    }

    pub(super) fn notes(&self, ch: usize) -> &[Note] {
        self.project.patterns[self.project.current_pattern].notes.get(ch).map_or(&[], |v| v.as_slice())
    }

    /// Trie les notes et retrouve la sélection après le tri.
    pub(super) fn finish_note_edit(&mut self, ch: usize, label: &str) {
        let selected: Vec<Note> = self.piano.selected.iter().filter_map(|&i| self.notes(ch).get(i).copied()).collect();
        let notes = self.notes_mut(ch);
        notes.sort_by_key(|n| (n.start, n.key));
        let mut taken = vec![false; notes.len()];
        let mut sel = Vec::new();
        for s in selected {
            if let Some(i) = notes.iter().enumerate().position(|(i, n)| !taken[i] && *n == s) {
                taken[i] = true;
                sel.push(i);
            }
        }
        self.piano.selected = sel;
        self.commit(label);
    }

    pub(super) fn piano_view(&mut self, c: &mut Canvas, input: &Input, header: Rect, body: Rect) {
        let t = self.theme;
        let Some(ch) = self.piano_channel() else {
            c.fill_rrect(body, 16.0, t.panel);
            self.empty_state(c, body, Icon::Piano, tr("Add an instrument to write notes"));
            return;
        };
        let pat = self.project.current_pattern;

        // En-tête : canal édité et grille.
        let chan_box = Rect::new(header.x, header.y, 220.0, header.h);
        c.fill_rrect(chan_box, 10.0, t.raised);
        c.stroke_rrect(chan_box, 10.0, 1.0, t.hairline);
        let n = self.project.channels.len();
        if self.icon_button(c, input, wid("pr-prev", 0, 0), Rect::new(chan_box.x + 2.0, chan_box.y + 2.0, 28.0, chan_box.h - 4.0), Icon::ChevronLeft, false, (tr("Previous channel"), "")) {
            self.focus = Focus::Channel((ch + n - 1) % n);
            self.piano.selected.clear();
        }
        if self.icon_button(c, input, wid("pr-next", 0, 0), Rect::new(chan_box.right() - 30.0, chan_box.y + 2.0, 28.0, chan_box.h - 4.0), Icon::ChevronRight, false, (tr("Next channel"), "")) {
            self.focus = Focus::Channel((ch + 1) % n);
            self.piano.selected.clear();
        }
        let name = self.project.channels[ch].name.clone();
        let (bx, by) = chan_box.center();
        let label = self.fit(self.f.sans_med, 13.0, &name, chan_box.w - 70.0);
        self.text_center(c, self.f.sans_med, 13.0, bx, by, &label, t.text);
        let name_r = Rect::new(chan_box.x + 32.0, chan_box.y, chan_box.w - 64.0, chan_box.h);
        if input.inside && name_r.contains(input.x, input.y) && self.drag.is_none() {
            self.set_tip(tr("Channel\nDouble-click: rename"), "");
            if input.double {
                self.open_popup(Popup::Rename { target: Rename::Channel(ch), text: name.clone() }, chan_box);
            }
        }

        // Gamme et accords : coller à la gamme, tampon d'accord et son type.
        let scaled = self.project.scale != ScaleKind::Chromatic;
        let mut hx = chan_box.right() + 12.0;
        let tip = if scaled { (tr("Snap to scale"), "") } else { (tr("Snap to scale\nPick a scale first, at the top"), "") };
        if self.icon_button(c, input, wid("pr-scale", 0, 0), Rect::new(hx, header.y, 36.0, header.h), Icon::Scale, self.piano.scale_snap, tip) {
            self.piano.scale_snap = !self.piano.scale_snap;
        }
        hx += 40.0;
        let stamp = self.piano.chord_stamp;
        if self.icon_button(c, input, wid("pr-chord", 0, 0), Rect::new(hx, header.y, 36.0, header.h), Icon::Chord, stamp, (tr("Stamp chords\nOne click: a whole chord"), "")) {
            self.piano.chord_stamp = !stamp;
        }
        hx += 38.0;
        let chip = Rect::new(hx, header.y + 4.0, 64.0, header.h - 8.0);
        let cid = wid("pr-chord-kind", 0, 0);
        let chov = self.hovered(input, cid, chip);
        let chv = self.hover_anim(cid, chov);
        let on = self.anim.to(cid ^ 0x4f4e, stamp as u8 as f32, T_HOVER);
        c.fill_rrect(chip, 8.0, t.raised.mix(t.raised_hi, chv).alpha(0.5 + 0.5 * on.max(chv)));
        let in_scale = self.project.scale != ScaleKind::Chromatic;
        let label = if in_scale { scale_shape(self.piano.chord).1 } else { self.piano.chord.map_or("M", chord_symbol) };
        let (kx, ky) = chip.center();
        self.text_center(c, self.f.sans_med, 12.0, kx - 5.0, ky, label, t.muted.mix(t.text, on.max(chv * 0.6)));
        draw_icon(c, Icon::ChevronDown, chip.right() - 10.0, ky, 9.0, t.muted);
        if chov {
            let inv = tr(INVERSIONS[usize::from(self.piano.chord_inv.min(3))]);
            let tip = if in_scale {
                tr_fmt!("Chord · {} · {}\nFollows the scale · wheel: inversion", tr(scale_shape(self.piano.chord).0), inv)
            } else {
                tr_fmt!("Chord · {} · {}\nWheel: inversion", tr(chord_name(self.piano.chord.unwrap_or(ChordKind::Major))), inv)
            };
            self.set_tip(&tip, "");
        }
        if chov && input.pressed {
            self.open_popup(Popup::Chord, chip);
        }
        if chov && input.wheel != 0.0 {
            let inv = (i32::from(self.piano.chord_inv) + input.wheel.signum() as i32).clamp(0, 3);
            self.piano.chord_inv = inv as u8;
            self.piano.chord_stamp = true;
        }
        if self.piano.chord_inv > 0 {
            c.fill_circle(chip.right() - 4.0, chip.y + 4.0, 2.5, t.accent);
        }

        hx += chip.w + 12.0;
        let hum = Rect::new(hx, header.y, 36.0, header.h);
        if self.icon_button(c, input, wid("pr-humanize", 0, 0), hum, Icon::Dice, false, (tr("Humanize\nThe selection, or else the whole channel"), "")) {
            self.open_popup(Popup::Humanize, hum);
        }
        let tools = Rect::new(hum.right() + 4.0, header.y, 36.0, header.h);
        let tools_open = matches!(self.popup.as_ref().map(|p| &p.kind), Some(Popup::Tools | Popup::Tool(_)));
        let tip = (tr("MIDI tools\nArpeggiate, strum, invert… and generators"), "");
        if self.icon_button(c, input, wid("pr-tools", 0, 0), tools, Icon::Wand, tools_open, tip) {
            self.open_popup(Popup::Tools, tools);
        }

        let snap_box = Rect::new(header.right() - 236.0, header.y, 236.0, header.h);
        let snap_sel = SNAPS.iter().position(|&s| s == self.piano.snap).unwrap_or(2);
        let tips = [(tr("Snap to 1/4"), ""), (tr("Snap to 1/8"), ""), (tr("Snap to 1/16"), ""), (tr("Snap to 1/32"), "")];
        if let Some(i) = self.segmented_tips(c, input, wid("pr-snap", 0, 0), snap_box, &["1/4", "1/8", "1/16", "1/32"], &tips, snap_sel) {
            self.piano.snap = SNAPS[i];
        }
        draw_icon(c, Icon::Magnet, snap_box.x - 18.0, header.y + header.h * 0.5, 15.0, t.muted);
        let help = Rect::new(snap_box.x - 74.0, header.y, 36.0, header.h);
        self.icon_button(c, input, wid("pr-help", 0, 0), help, Icon::Info, false, (tr(PIANO_HELP), ""));

        c.fill_rrect(body, 16.0, t.panel);
        c.stroke_rrect(body, 16.0, 1.0, t.hairline);
        let inner = body.inset(12.0);
        let keys = Rect::new(inner.x, inner.y + RULER, KEYBOARD, inner.h - RULER - VELOCITY - LANE - 16.0);
        let grid = Rect::new(keys.right(), keys.y, inner.w - KEYBOARD, keys.h);
        let ruler = Rect::new(grid.x, inner.y, grid.w, RULER);
        self.tour_anchor(1, grid);
        let vel_lane = Rect::new(grid.x, grid.bottom() + 8.0, grid.w, VELOCITY);
        let note_lane = Rect::new(grid.x, vel_lane.bottom() + 8.0, grid.w, LANE);
        let pat_len = self.project.patterns[pat].len.max(TICKS_PER_BAR) as f32;

        // Cadrage initial sur le pattern et les notes du canal.
        if self.piano.fitted_for != Some((pat, ch)) {
            self.piano.fitted_for = Some((pat, ch));
            self.piano.zoom = grid.w / pat_len;
            self.piano.scroll_x = 0.0;
            let notes = self.notes(ch);
            let center = if notes.is_empty() {
                self.project.channels[ch].root_note as f32
            } else {
                notes.iter().map(|n| n.key as f32).sum::<f32>() / notes.len() as f32
            };
            self.piano.scroll_y = (center + grid.h / KEY_H * 0.5).round().clamp(grid.h / KEY_H, 127.0);
        }

        // Bouton du milieu : faire glisser la vue dans les deux sens.
        let pan_id = wid("pr-pan", 0, 0);
        if input.inside && grid.contains(input.x, input.y) && input.middle_pressed {
            self.pan = Some((pan_id, input.x, input.y, self.piano.scroll_x, self.piano.scroll_y));
        }
        if let Some((_, x0, y0, sx0, sy0)) = self.pan.filter(|p| p.0 == pan_id) {
            self.piano.scroll_x = (sx0 - (input.x - x0) / self.piano.zoom).max(0.0);
            self.piano.scroll_y = (sy0 + (input.y - y0) / KEY_H).clamp(grid.h / KEY_H, 127.0);
            self.anim.set(wid("pr-sx", 0, 0), self.piano.scroll_x);
            self.anim.set(wid("pr-sy", 0, 0), self.piano.scroll_y);
        }
        // Molette : défilement vertical, Maj horizontal, Ctrl zoom.
        if input.inside && (grid.contains(input.x, input.y) || keys.contains(input.x, input.y)) && input.wheel != 0.0 {
            if input.mods.ctrl {
                let at = self.piano.scroll_x + (input.x - grid.x) / self.piano.zoom;
                self.piano.zoom = (self.piano.zoom * 1.15f32.powf(input.wheel)).clamp(0.05, 8.0);
                self.piano.scroll_x = (at - (input.x - grid.x) / self.piano.zoom).max(0.0);
            } else if input.mods.shift {
                self.piano.scroll_x = (self.piano.scroll_x - input.wheel * 60.0 / self.piano.zoom).max(0.0);
            } else {
                self.piano.scroll_y = (self.piano.scroll_y + input.wheel * 3.0).clamp(grid.h / KEY_H, 127.0);
            }
        }
        let sx = self.anim.to(wid("pr-sx", 0, 0), self.piano.scroll_x, T_LAYOUT);
        let sy = self.anim.to(wid("pr-sy", 0, 0), self.piano.scroll_y, T_LAYOUT);
        let zoom = self.piano.zoom;
        let x_of = |tick: f32| grid.x + (tick - sx) * zoom;
        let y_of = |key: f32| grid.y + (sy - key) * KEY_H;
        let tick_at = |x: f32| sx + (x - grid.x) / zoom;
        let key_at = |y: f32| (sy - ((y - grid.y) / KEY_H).floor()).clamp(0.0, 127.0);

        // Fond : rangées, puis lignes de pas, temps, mesures.
        let prev = c.push_clip(grid);
        let top_key = sy.floor() as i32;
        let rows = (grid.h / KEY_H).ceil() as i32 + 2;
        let scaled = self.project.scale != ScaleKind::Chromatic;
        for k in (top_key - rows..=top_key + 1).filter(|k| (0..128).contains(k)) {
            let y = y_of(k as f32);
            let col = if is_black(k as u8) { t.well } else { t.panel.mix(t.raised, 0.35) };
            c.fill_rect(Rect::new(grid.x, y, grid.w, KEY_H), col);
            // Gamme du projet : rangées hors gamme assombries, tonique teintée.
            if scaled {
                if !self.project.in_scale(k as u8) {
                    c.fill_rect(Rect::new(grid.x, y, grid.w, KEY_H), t.bg_deep.alpha(0.55));
                } else if (k as u8 + 12 - self.project.key % 12) % 12 == 0 {
                    c.fill_rect(Rect::new(grid.x, y, grid.w, KEY_H), t.accent.alpha(0.05));
                }
            }
            if k % 12 == 0 {
                c.fill_rect(Rect::new(grid.x, y + KEY_H - 1.0, grid.w, 1.0), t.hairline.alpha(2.0));
            }
        }
        let first = (sx / TICKS_PER_STEP as f32).floor() as u32;
        let last = ((sx + grid.w / zoom) / TICKS_PER_STEP as f32).ceil() as u32;
        let step_px = TICKS_PER_STEP as f32 * zoom;
        for s in first..=last {
            let tick = s * TICKS_PER_STEP;
            let x = x_of(tick as f32).round();
            let (w, a) = if tick % TICKS_PER_BAR == 0 {
                (1.0, 3.5)
            } else if tick % 96 == 0 {
                (1.0, 1.8)
            } else if step_px >= 8.0 {
                (1.0, 0.7)
            } else {
                continue;
            };
            c.fill_rect(Rect::new(x, grid.y, w, grid.h), t.hairline.alpha(a));
        }
        let end_x = x_of(pat_len);
        if end_x < grid.right() {
            c.fill_rect(Rect::new(end_x, grid.y, grid.right() - end_x, grid.h), t.bg_deep.alpha(0.55));
        }

        // Notes fantômes des autres canaux.
        for (other, notes) in self.project.patterns[pat].notes.iter().enumerate() {
            if other == ch || self.project.channels.get(other).is_none() {
                continue;
            }
            for n in notes {
                let r = Rect::new(x_of(n.start as f32), y_of(n.key as f32) + 2.0, (n.len as f32 * zoom).max(3.0), KEY_H - 4.0);
                if r.right() > grid.x && r.x < grid.right() {
                    c.stroke_rrect(r, 3.0, 1.0, t.text.alpha(0.1));
                }
            }
        }

        // Interaction dans la grille.
        let grid_id = wid("pr-grid", 0, 0);
        let over_grid = input.inside && grid.contains(input.x, input.y) && (self.drag.is_none() || self.is_dragging(grid_id));
        let hit = |app: &App, x: f32, y: f32| -> Option<(usize, bool)> {
            let key = key_at(y) as u8;
            let tick = tick_at(x);
            app.notes(ch).iter().enumerate().rev().find_map(|(i, n)| {
                let x1 = x_of((n.start + n.len) as f32);
                let inside = n.key == key && tick >= n.start as f32 && x <= x1.max(x_of(n.start as f32) + 4.0);
                inside.then_some((i, x1 - x <= EDGE))
            })
        };
        let hover_note = if over_grid { hit(self, input.x, input.y) } else { None };

        if over_grid && input.pressed && self.drag.is_none() {
            self.begin_drag(grid_id, input, 0.0);
            let tick = tick_at(input.x);
            let key = key_at(input.y);
            match hover_note {
                Some((i, edge)) => {
                    if !self.piano.selected.contains(&i) {
                        if !input.mods.shift {
                            self.piano.selected.clear();
                        }
                        self.piano.selected.push(i);
                    }
                    let orig: Vec<(usize, Note)> = self.piano.selected.iter().filter_map(|&j| self.notes(ch).get(j).map(|n| (j, *n))).collect();
                    let note = self.notes(ch)[i];
                    self.piano.last_len = note.len;
                    if edge {
                        self.piano_drag = Some(PianoDrag::Resize { orig, tick0: tick });
                    } else {
                        self.preview_note(ch, note.key);
                        self.piano_drag = Some(PianoDrag::Move { orig, tick0: tick, key0: key });
                    }
                }
                None if input.mods.ctrl => {
                    self.piano_drag = Some(PianoDrag::Select { x0: input.x, y0: input.y });
                }
                None => {
                    let snap = self.piano.snap as f32;
                    let start = ((tick / snap).floor() * snap).max(0.0) as u32;
                    let len = self.piano.last_len.max(1);
                    let keys = self.stamp_keys(key as u8);
                    let notes = self.notes_mut(ch);
                    let base = notes.len();
                    notes.extend(keys.iter().map(|&k| Note { start, len, key: k, vel: 0.8, ..Note::DEFAULT }));
                    let orig: Vec<(usize, Note)> = (base..notes.len()).map(|i| (i, notes[i])).collect();
                    for &k in &keys {
                        self.anim.set(pop_id(k, start), 0.0);
                    }
                    self.piano.selected = (base..base + keys.len()).collect();
                    self.preview_note(ch, keys[0]);
                    self.piano_drag = Some(PianoDrag::Move { orig, tick0: tick, key0: key });
                    self.commit(if keys.len() > 1 { tr("Place chord") } else { tr("Place note") });
                }
            }
        }
        // Clic droit maintenu : efface les notes balayées.
        if over_grid && input.right_down && let Some((i, _)) = hover_note {
            let gone = self.notes_mut(ch).remove(i);
            self.piano.erased.push((gone, self.time));
            self.piano.selected.clear();
            self.commit(tr("Delete notes"));
        }

        // Geste en cours.
        if self.is_dragging(grid_id) {
            let snap = self.piano.snap as f32;
            match self.piano_drag.clone() {
                Some(PianoDrag::Move { orig, tick0, key0 }) => {
                    let dt = ((tick_at(input.x) - tick0) / snap).round() * snap;
                    let dk = key_at(input.y) - key0;
                    let min_start = orig.iter().map(|(_, n)| n.start).min().unwrap_or(0) as f32;
                    let dt = dt.max(-min_start);
                    let mut moved_key = None;
                    let scale_snap = self.piano.scale_snap;
                    for &(i, n) in &orig {
                        let key = (n.key as f32 + dk).clamp(0.0, 127.0) as u8;
                        let key = if scale_snap && dk != 0.0 { self.project.snap(key) } else { key };
                        if let Some(dst) = self.notes_mut(ch).get_mut(i) {
                            *dst = Note { start: (n.start as f32 + dt) as u32, key, ..n };
                        }
                        moved_key = Some(key);
                    }
                    if let (Some(k), Some((pc, pk))) = (moved_key, self.preview)
                        && orig.len() == 1
                        && pc == ch
                        && pk != k
                    {
                        self.preview_note(ch, k);
                    }
                    self.commit(tr("Move notes"));
                }
                Some(PianoDrag::Resize { orig, tick0 }) => {
                    let dt = ((tick_at(input.x) - tick0) / snap).round() * snap;
                    for &(i, n) in &orig {
                        let len = (n.len as f32 + dt).max(snap.min(n.len as f32)).max(1.0) as u32;
                        if let Some(dst) = self.notes_mut(ch).get_mut(i) {
                            dst.len = len;
                        }
                        self.piano.last_len = len;
                    }
                    self.commit(tr("Note length"));
                }
                Some(PianoDrag::Select { x0, y0 }) => {
                    let sel = Rect::new(x0.min(input.x), y0.min(input.y), (x0 - input.x).abs(), (y0 - input.y).abs());
                    c.fill_rect(sel, t.select.alpha(0.12));
                    c.stroke_rrect(sel, 2.0, 1.0, t.select.alpha(0.8));
                    let (t0, t1) = (tick_at(sel.x), tick_at(sel.right()));
                    let (k0, k1) = (key_at(sel.bottom()), key_at(sel.y));
                    self.piano.selected = self
                        .notes(ch)
                        .iter()
                        .enumerate()
                        .filter(|(_, n)| {
                            (n.start + n.len) as f32 > t0 && (n.start as f32) < t1 && (n.key as f32) >= k0 && (n.key as f32) <= k1
                        })
                        .map(|(i, _)| i)
                        .collect();
                }
                _ => {}
            }
            if input.released {
                if matches!(self.piano_drag, Some(PianoDrag::Move { .. } | PianoDrag::Resize { .. })) {
                    self.finish_note_edit(ch, tr("Edit notes"));
                }
                self.piano_drag = None;
            }
        }

        // Rangée survolée : un léger bandeau guide l'œil vers la hauteur visée.
        if over_grid && self.drag.is_none() {
            let row = Rect::new(grid.x, y_of(key_at(input.y)), grid.w, KEY_H);
            c.fill_rect(row, t.text.alpha(0.035));
        }

        // Notes du canal.
        let melodic_labels = KEY_H >= 14.0;
        for (i, n) in self.notes(ch).to_vec().into_iter().enumerate() {
            let r = Rect::new(x_of(n.start as f32), y_of(n.key as f32) + 1.0, (n.len as f32 * zoom).max(4.0), KEY_H - 2.0);
            if r.right() < grid.x || r.x > grid.right() || r.bottom() < grid.y || r.y > grid.bottom() {
                continue;
            }
            let selected = self.piano.selected.contains(&i);
            let hov = hover_note.is_some_and(|(h, _)| h == i) && self.drag.is_none();
            let hv = self.hover_anim(wid("pr-note", i, 0), hov);
            // Note qui vient d'être posée : éclat bref et onde qui s'élargit.
            let pid = pop_id(n.key, n.start);
            let pop = if self.anim.get(pid).is_some() { self.anim.to(pid, 1.0, 0.09) } else { 1.0 };
            if pop < 1.0 {
                let grow = 2.0 + pop * 9.0;
                c.stroke_rrect(r.inset(-grow), 4.0 + grow, 1.5, t.accent.alpha((1.0 - pop) * 0.8));
            }
            if hv > 0.01 {
                c.fill_rrect(r.inset(-2.5), 6.0, t.accent.alpha(0.18 * hv));
            }
            let base = t.accent_deep.mix(t.accent, 0.4 + 0.6 * n.vel);
            let fill = if selected { base.mix(t.accent_hot, 0.35) } else { base.mix(t.accent_hot, 0.3 * hv) };
            let fill = fill.mix(WHITE, (1.0 - pop) * 0.55);
            let body = r.inset(-(1.0 - pop) * 1.5);
            let chance = f32::from(n.prob.min(100)) / 100.0;
            c.fill_rrect(body, 4.0, fill.alpha(0.15 + 0.85 * chance));
            if n.prob < 100 {
                c.stroke_rrect(body, 4.0, 1.0, fill);
            }
            if n.ratchet > 1 && r.w > 8.0 {
                for j in 1..n.ratchet {
                    let x = (r.x + r.w * f32::from(j) / f32::from(n.ratchet)).round();
                    c.fill_rect(Rect::new(x, r.y + 2.0, 1.0, r.h - 4.0), t.bg_deep.alpha(0.75));
                }
            }
            if n.cond != ponzik_engine::NoteCond::Always {
                c.fill_circle(r.right() - 3.5, r.y + 3.5, 2.5, t.select);
            }
            if selected {
                c.stroke_rrect(r, 4.0, 1.5, t.select);
            } else if hv > 0.01 {
                c.stroke_rrect(r, 4.0, 1.0, t.text.alpha(0.6 * hv));
            }
            // Bord droit survolé : poignée de durée et flèches.
            if hover_note.is_some_and(|(h, e)| h == i && e) {
                c.fill_rrect(Rect::new(r.right() - 4.0, r.y + 2.0, 3.0, r.h - 4.0), 1.5, t.on_accent.alpha(0.85));
                let (ax, ay) = (r.right() + 7.0, r.y + r.h * 0.5);
                let mut arrows = ponzik_ui::Path::new();
                arrows.polygon(&[(ax - 3.0, ay), (ax + 1.0, ay - 3.5), (ax + 1.0, ay + 3.5)]);
                arrows.polygon(&[(ax + 9.0, ay), (ax + 5.0, ay - 3.5), (ax + 5.0, ay + 3.5)]);
                c.fill_path(&arrows, t.text.alpha(0.8));
            }
            if melodic_labels && r.w > 30.0 {
                self.text(c, self.f.mono_med, 10.0, r.x + 5.0, r.y + r.h * 0.5, &note_name(n.key), t.on_accent);
            }
        }
        // Curseur : poignée de durée, note à saisir (saisie pendant le geste), pose dans le vide.
        let gesture = if self.is_dragging(grid_id) && input.down { self.piano_drag.as_ref() } else { None };
        match (gesture, hover_note) {
            (Some(PianoDrag::Resize { .. }), _) | (None, Some((_, true))) => self.set_cursor(Cursor::ResizeH),
            (Some(PianoDrag::Move { .. }), _) => self.set_cursor(Cursor::Grab),
            (None, Some(_)) => self.set_cursor(Cursor::Move),
            _ if over_grid => self.set_cursor(Cursor::Crosshair),
            _ => {}
        }
        // L'index peut viser une note effacée par le clic droit de cette image.
        if let Some((i, edge)) = hover_note
            && self.drag.is_none()
            && let Some(&n) = self.notes(ch).get(i)
        {
            let what = if edge { tr("Length · drag") } else { tr("Drag: move · right-click: erase") };
            self.set_tip(&format!("{} · {}", note_name(n.key), what), "");
        }

        // Notes effacées : elles rétrécissent et s'éteignent.
        let now = self.time;
        self.piano.erased.retain(|&(_, at)| now - at < 0.22);
        if !self.piano.erased.is_empty() {
            self.anim.set(wid("pr-erase", 0, 0), now);
        }
        for &(n, at) in &self.piano.erased {
            let k = ((now - at) / 0.22).clamp(0.0, 1.0);
            let r = Rect::new(x_of(n.start as f32), y_of(n.key as f32) + 1.0, (n.len as f32 * zoom).max(4.0), KEY_H - 2.0);
            let shrink = r.inset(k * r.h * 0.35);
            c.fill_rrect(shrink, 4.0, t.negative.mix(t.accent, 0.3).alpha(0.7 * (1.0 - k)));
        }

        // Fantôme de ce qu'un clic poserait : une note, ou l'accord du tampon.
        if over_grid && hover_note.is_none() && self.drag.is_none() && !input.mods.ctrl {
            let snap = self.piano.snap as f32;
            let start = ((tick_at(input.x) / snap).floor() * snap).max(0.0);
            let w = (self.piano.last_len.max(1) as f32 * zoom).max(4.0);
            let strong = self.piano.chord_stamp || self.piano.scale_snap;
            for k in self.stamp_keys(key_at(input.y) as u8) {
                let r = Rect::new(x_of(start), y_of(k as f32) + 1.0, w, KEY_H - 2.0);
                c.fill_rrect(r, 4.0, t.accent.alpha(if strong { 0.12 } else { 0.07 }));
                c.stroke_rrect(r, 4.0, 1.0, t.accent.alpha(if strong { 0.6 } else { 0.35 }));
            }
        }

        // Tête de lecture.
        if self.playing && self.project.mode == PlayMode::Pattern(pat) {
            let x = x_of(self.cycle_position() as f32);
            c.fill_rect(Rect::new(x.round() - 1.0, grid.y, 2.0, grid.h), t.select);
        }
        c.set_clip(Some(prev));

        // Raccourcis d'édition.
        if self.popup.is_none() && !self.sidebar_focus {
            self.piano_keys(input, ch);
        }

        // Clavier : touches blanches pleine largeur, noires par-dessus ; clic pour écouter.
        let prev = c.push_clip(keys);
        let ivory = t.text.mix(t.raised_hi, 0.12);
        c.fill_rect(keys, ivory);
        let visible: Vec<i32> = (top_key - rows..=top_key + 1).filter(|k| (0..128).contains(k)).collect();
        let hover_key = (input.inside && keys.contains(input.x, input.y) && self.drag.is_none()).then(|| key_at(input.y) as u8);
        for &k in &visible {
            let key = k as u8;
            let y = y_of(k as f32);
            let r = Rect::new(keys.x, y, keys.w, KEY_H);
            let playing = self.preview == Some((ch, key));
            let hov = hover_key == Some(key);
            if is_black(key) {
                let fill = if playing { t.accent } else { t.bg_deep.mix(t.raised_hi, hov as u8 as f32 * 0.6) };
                c.fill_rrect(Rect::new(r.x, r.y + 1.0, r.w * 0.6, r.h - 2.0), 3.0, fill);
            } else {
                if playing || hov {
                    c.fill_rect(Rect::new(r.x, r.y, r.w, r.h), if playing { t.accent } else { ivory.mix(t.muted, 0.25) });
                }
                // Séparation entre deux blanches voisines (Mi-Fa, Si-Do).
                if matches!(key % 12, 0 | 5) {
                    c.fill_rect(Rect::new(r.x, r.bottom() - 1.0, r.w, 1.0), t.bg_deep.alpha(0.35));
                }
            }
            if key % 12 == 0 {
                self.text_right(c, self.f.mono_med, 10.0, r.right() - 6.0, r.y + r.h * 0.5, &note_name(key), t.bg_deep.alpha(0.7));
            }
        }
        let kid = wid("pr-keys", 0, 0);
        if self.hovered(input, kid, keys) && input.pressed {
            self.preview_note(ch, key_at(input.y) as u8);
            self.begin_drag(kid, input, 0.0);
        }
        c.fill_rect(Rect::new(keys.right() - 1.0, keys.y, 1.0, keys.h), t.bg_deep);
        c.set_clip(Some(prev));

        // Règle : mesures ; clic = marqueur de départ, glisser = sélection de temps.
        let prev = c.push_clip(Rect::new(ruler.x, ruler.y, ruler.w, grid.bottom() - ruler.y));
        let bar_first = (sx / TICKS_PER_BAR as f32).floor() as u32;
        let bar_last = ((sx + grid.w / zoom) / TICKS_PER_BAR as f32).ceil() as u32;
        for b in bar_first..=bar_last {
            let x = x_of((b * TICKS_PER_BAR) as f32);
            c.fill_rect(Rect::new(x.round(), ruler.y + 14.0, 1.0, 14.0), t.hairline.alpha(3.0));
            self.text(c, self.f.mono, 11.0, x + 6.0, ruler.y + 18.0, &(b + 1).to_string(), t.text2);
            for beat in 1..4 {
                let bx = x_of((b * TICKS_PER_BAR + beat * 96) as f32).round();
                c.fill_rect(Rect::new(bx, ruler.y + 22.0, 1.0, 6.0), t.hairline.alpha(2.0));
            }
        }
        let snap = self.piano.snap as f32;
        if let Some((a, b)) = self.ruler(c, input, View::Piano, ruler, grid, &|t: f32| (t / snap).round() * snap, &x_of, &tick_at) {
            // La sélection de temps sélectionne aussi les notes qui y commencent.
            self.piano.selected = self
                .notes(ch)
                .iter()
                .enumerate()
                .filter(|(_, n)| n.start >= a && n.start < b)
                .map(|(i, _)| i)
                .collect();
        }
        c.set_clip(Some(prev));

        // Vélocités : glisser dans le couloir pour les dessiner.
        c.fill_rrect(vel_lane, 10.0, t.well);
        let prev = c.push_clip(vel_lane);
        let vid = wid("pr-vel", 0, 0);
        if self.hovered(input, vid, vel_lane) || self.is_dragging(vid) {
            self.set_cursor(Cursor::ResizeV);
        }
        if self.hovered(input, vid, vel_lane) && input.pressed {
            self.begin_drag(vid, input, 0.0);
            self.piano_drag = Some(PianoDrag::Velocity);
        }
        if self.is_dragging(vid) {
            let tick = tick_at(input.x);
            let v = ((vel_lane.bottom() - 6.0 - input.y) / (vel_lane.h - 12.0)).clamp(0.05, 1.0);
            let only_selected = !self.piano.selected.is_empty();
            let sel = self.piano.selected.clone();
            let notes = self.notes_mut(ch);
            let mut changed = false;
            for (i, n) in notes.iter_mut().enumerate() {
                let near = (n.start as f32 - tick).abs() * zoom < 6.0;
                if near && (!only_selected || sel.contains(&i)) && n.vel != v {
                    n.vel = v;
                    changed = true;
                }
            }
            if changed {
                self.commit(tr("Velocity"));
            }
        }
        for (i, n) in self.notes(ch).to_vec().into_iter().enumerate() {
            let x = x_of(n.start as f32).round();
            if x < vel_lane.x - 4.0 || x > vel_lane.right() {
                continue;
            }
            let h = (vel_lane.h - 12.0) * n.vel;
            let col = if self.piano.selected.contains(&i) { t.select } else { t.accent.mix(t.accent_deep, 1.0 - n.vel) };
            c.fill_rrect(Rect::new(x, vel_lane.bottom() - 6.0 - h, 3.0, h), 1.5, col);
            c.fill_circle(x + 1.5, vel_lane.bottom() - 6.0 - h, 3.0, col);
        }
        c.set_clip(Some(prev));
        draw_icon(c, Icon::Mixer, inner.x + KEYBOARD * 0.5, vel_lane.y + vel_lane.h * 0.5, 15.0, t.muted.alpha(0.7));
        self.tip_zone(input, Rect::new(inner.x, vel_lane.y, KEYBOARD, vel_lane.h), tr("Velocity\nDrag in the lane to draw it"), "");
        self.note_lane(c, input, ch, Rect::new(inner.x, note_lane.y, KEYBOARD - 6.0, LANE), note_lane, &x_of, &tick_at);
    }

    /// Lane of chances, conditions or repeats: three tabs on the left, then
    /// a mark under each note, drawn by dragging (the selection, or the notes
    /// under the pointer).
    #[allow(clippy::too_many_arguments)]
    fn note_lane(&mut self, c: &mut Canvas, input: &Input, ch: usize, tabs: Rect, lane: Rect, x_of: &dyn Fn(f32) -> f32, tick_at: &dyn Fn(f32) -> f32) {
        use super::tools::{COND_LABELS, CONDS, cond_index, cond_label};
        let t = self.theme;
        // Tabs: chance, condition, repeats.
        let labels = ["%", "N:M", "×"];
        let tips = [
            tr("Chance\nHow often each note plays · drag in the lane"),
            tr("Condition\nOn which passes of the pattern a note plays: 1:2 is every other pass · drag in the lane"),
            tr("Repeats\nRatchets: the note fires several times within its length · drag in the lane"),
        ];
        let th = tabs.h / 3.0;
        for (i, label) in labels.iter().enumerate() {
            let r = Rect::new(tabs.x, tabs.y + i as f32 * th, tabs.w, th - 2.0);
            let id = wid("pr-lane-tab", i, 0);
            let hov = self.hovered(input, id, r);
            let hv = self.hover_anim(id, hov);
            let on = self.anim.to(id ^ 0x6f6e, (self.piano.lane == i) as u8 as f32, T_HOVER);
            c.fill_rrect(r, 6.0, t.raised.mix(t.raised_hi, hv).alpha(0.4 + 0.6 * hv.max(on)).mix(t.accent, on * 0.85));
            let (cx, cy) = r.center();
            self.text_center(c, self.f.mono_med, 11.0, cx, cy, label, t.muted.mix(t.text, hv).mix(t.on_accent, on));
            if hov {
                self.set_tip(tips[i], "");
                self.set_cursor(Cursor::Hand);
                if input.pressed {
                    self.piano.lane = i;
                }
            }
        }

        c.fill_rrect(lane, 10.0, t.well);
        let prev = c.push_clip(lane);
        let id = wid("pr-note-lane", 0, 0);
        let hov = self.hovered(input, id, lane);
        if hov || self.is_dragging(id) {
            self.set_cursor(Cursor::ResizeV);
        }
        if hov && input.pressed {
            self.begin_drag(id, input, 0.0);
        }
        let mode = self.piano.lane.min(2);
        let level = ((lane.bottom() - 6.0 - input.y) / (lane.h - 12.0)).clamp(0.0, 1.0);
        if self.is_dragging(id) {
            let tick = tick_at(input.x);
            let zoom = self.piano.zoom;
            let only_selected = !self.piano.selected.is_empty();
            let sel = self.piano.selected.clone();
            let mut changed = false;
            for (i, n) in self.notes_mut(ch).iter_mut().enumerate() {
                let near = (n.start as f32 - tick).abs() * zoom < 6.0;
                if !near || (only_selected && !sel.contains(&i)) {
                    continue;
                }
                let before = *n;
                match mode {
                    0 => n.prob = ((level * 20.0).round() * 5.0) as u8,
                    1 => n.cond = CONDS[((level * (CONDS.len() - 1) as f32).round() as usize).min(CONDS.len() - 1)],
                    _ => n.ratchet = (level * 7.0).round() as u8 + 1,
                }
                changed |= *n != before;
            }
            if changed {
                let label = [tr("Note chance"), tr("Note condition"), tr("Note repeats")][mode];
                self.commit(label);
            }
            // What the drag sets, next to the pointer.
            let shown = match mode {
                0 => format!("{}%", (level * 20.0).round() as u32 * 5),
                1 => tr(COND_LABELS[((level * (CONDS.len() - 1) as f32).round() as usize).min(CONDS.len() - 1)]).to_string(),
                _ => format!("×{}", (level * 7.0).round() as u32 + 1),
            };
            self.text(c, self.f.mono_med, 11.0, (input.x + 10.0).min(lane.right() - 48.0), lane.y + 12.0, &shown, t.text);
        }
        let h = lane.h - 12.0;
        for (i, n) in self.notes(ch).to_vec().into_iter().enumerate() {
            let x = x_of(n.start as f32).round();
            if x < lane.x - 4.0 || x > lane.right() {
                continue;
            }
            let selected = self.piano.selected.contains(&i);
            let col = if selected { t.select } else { t.accent };
            match mode {
                0 => {
                    let v = f32::from(n.prob.min(100)) / 100.0;
                    let top = lane.bottom() - 6.0 - h * v;
                    c.fill_rrect(Rect::new(x, top, 3.0, h * v), 1.5, col.alpha(0.35 + 0.65 * v));
                    c.fill_circle(x + 1.5, top, 3.0, col);
                    if n.prob < 100 {
                        self.text(c, self.f.mono, 10.0, x + 6.0, top.max(lane.y + 10.0), &format!("{}", n.prob), t.text2);
                    }
                }
                1 => {
                    let k = cond_index(n.cond);
                    let y = lane.bottom() - 6.0 - h * k as f32 / (CONDS.len() - 1) as f32;
                    if n.cond == ponzik_engine::NoteCond::Always {
                        c.fill_circle(x + 1.5, y, 2.5, col.alpha(0.45));
                    } else {
                        let label = cond_label(n.cond);
                        let w = self.fonts.measure(self.f.mono_med, 10.0, &label) + 8.0;
                        let chip = Rect::new(x - 1.0, y - 7.0, w, 14.0);
                        c.fill_rrect(chip, 5.0, col.alpha(0.85));
                        self.text(c, self.f.mono_med, 10.0, chip.x + 4.0, y, &label, t.on_accent);
                    }
                }
                _ => {
                    let reps = n.ratchet.clamp(1, 8);
                    for k in 0..reps {
                        let y = lane.bottom() - 6.0 - (f32::from(k) + 1.0) * h / 8.0;
                        c.fill_rrect(Rect::new(x, y, 8.0, h / 8.0 - 1.5), 1.5, col.alpha(if reps > 1 { 0.95 } else { 0.4 }));
                    }
                    if reps > 1 {
                        let y = lane.bottom() - 6.0 - f32::from(reps) * h / 8.0;
                        self.text(c, self.f.mono, 10.0, x + 11.0, y.max(lane.y + 10.0), &format!("×{reps}"), t.text2);
                    }
                }
            }
        }
        c.set_clip(Some(prev));
    }

    /// Décale au hasard le début (±`ticks`) et la vélocité (±`vel`, relatif) des
    /// notes sélectionnées, ou de tout le canal.
    pub(super) fn humanize(&mut self, ticks: u32, vel: f32) {
        let Some(ch) = self.piano_channel() else { return };
        if self.notes(ch).is_empty() {
            self.toast(tr("No notes to humanize"));
            return;
        }
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64);
        let tool = &mut self.piano.tool;
        (tool.timing, tool.vel, tool.seed) = (ticks as f32 / TICKS_PER_STEP as f32, vel, nanos | 1);
        self.apply_tool(super::tools::Tool::Humanize);
    }

    /// Raccourcis d'édition du piano roll, ceux de FL Studio.
    fn piano_keys(&mut self, input: &Input, ch: usize) {
        let snap = self.piano.snap;
        for &(key, mods) in &input.keys {
            // Sélection explicite, sinon tout le canal.
            let targets: Vec<usize> =
                if self.piano.selected.is_empty() { (0..self.notes(ch).len()).collect() } else { self.piano.selected.clone() };
            match key {
                Key::Delete | Key::Backspace if !self.piano.selected.is_empty() => {
                    let mut sel = std::mem::take(&mut self.piano.selected);
                    sel.sort_unstable();
                    let notes = self.notes_mut(ch);
                    for i in sel.into_iter().rev() {
                        if i < notes.len() {
                            notes.remove(i);
                        }
                    }
                    self.commit(tr("Delete notes"));
                }
                Key::Char('a') if mods.ctrl => self.piano.selected = (0..self.notes(ch).len()).collect(),
                Key::Char('d') if mods.ctrl => self.piano.selected.clear(),
                Key::Char('c') if mods.ctrl && !mods.shift => {
                    let notes: Vec<Note> = targets.iter().filter_map(|&i| self.notes(ch).get(i).copied()).collect();
                    let first = notes.iter().map(|n| n.start).min().unwrap_or(0);
                    self.clipboard_notes = notes.into_iter().map(|n| Note { start: n.start - first, ..n }).collect();
                    let n = self.clipboard_notes.len();
                    self.toast(&tr_fmt_n!("{} note copied", "{} notes copied", n, n));
                }
                Key::Char('v') if mods.ctrl && !self.clipboard_notes.is_empty() => {
                    let at = ((self.start_marker as u32) / snap) * snap;
                    let clip = self.clipboard_notes.clone();
                    let notes = self.notes_mut(ch);
                    let base = notes.len();
                    for n in &clip {
                        notes.push(Note { start: n.start + at, ..*n });
                    }
                    self.piano.selected = (base..base + clip.len()).collect();
                    self.finish_note_edit(ch, tr("Paste notes"));
                }
                Key::Char('b') if mods.ctrl && !targets.is_empty() => {
                    let sel: Vec<Note> = targets.iter().filter_map(|&i| self.notes(ch).get(i).copied()).collect();
                    // Durée dupliquée : la sélection de temps si elle existe, sinon l'étendue des notes.
                    let span = match self.time_selection {
                        Some((View::Piano, a, b)) => b - a,
                        _ => {
                            let lo = sel.iter().map(|n| n.start).min().unwrap_or(0);
                            let hi = sel.iter().map(|n| n.start + n.len).max().unwrap_or(0);
                            (hi - lo).div_ceil(snap) * snap
                        }
                    };
                    let notes = self.notes_mut(ch);
                    let base = notes.len();
                    for n in &sel {
                        notes.push(Note { start: n.start + span, ..*n });
                    }
                    self.piano.selected = (base..base + sel.len()).collect();
                    if let Some((View::Piano, a, b)) = self.time_selection {
                        self.time_selection = Some((View::Piano, a + span, b + span));
                    }
                    self.finish_note_edit(ch, tr("Duplicate notes"));
                }
                Key::Char('q') if mods.ctrl && !targets.is_empty() => {
                    for &i in &targets {
                        if let Some(n) = self.notes_mut(ch).get_mut(i) {
                            n.start = ((n.start + snap / 2) / snap) * snap;
                        }
                    }
                    self.finish_note_edit(ch, tr("Quantize"));
                    self.toast(tr("Notes quantized"));
                }
                Key::Up | Key::Down if !self.piano.selected.is_empty() => {
                    let d: i32 = if mods.ctrl { 12 } else { 1 } * if key == Key::Up { 1 } else { -1 };
                    let sel = self.piano.selected.clone();
                    for i in sel {
                        if let Some(n) = self.notes_mut(ch).get_mut(i) {
                            n.key = (n.key as i32 + d).clamp(0, 127) as u8;
                        }
                    }
                    self.finish_note_edit(ch, tr("Transpose"));
                }
                Key::Left | Key::Right if !self.piano.selected.is_empty() => {
                    let d = if key == Key::Right { snap as i64 } else { -(snap as i64) };
                    let sel = self.piano.selected.clone();
                    for i in sel {
                        if let Some(n) = self.notes_mut(ch).get_mut(i) {
                            n.start = (n.start as i64 + d).max(0) as u32;
                        }
                    }
                    self.finish_note_edit(ch, tr("Nudge notes"));
                }
                _ => {}
            }
        }
    }
}
