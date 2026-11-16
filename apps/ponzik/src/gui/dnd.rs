//! Glisser-déposer depuis le navigateur : un instrument vers le rack, un effet
//! vers une piste ou un emplacement d'insert, un échantillon vers la playlist ou
//! un sampler. Un simple clic garde le comportement d'ajout direct.

use std::path::PathBuf;

use ponzik_engine::mixer::INSERT_SLOTS;
use ponzik_i18n::{tr, tr_fmt};
use ponzik_project::{EffectDef, InstrumentDef};
use ponzik_ui::{Canvas, Color, Rect};
use ponzik_window::Cursor;

use super::Input;
use super::app::{App, Focus, View};
use super::widgets::{Icon, draw_icon, wid};

/// Ce que l'on transporte.
#[derive(Clone, Debug)]
pub enum Payload {
    /// Type d'instrument (registre ou plugin CLAP) et nom du canal.
    Instrument { kind: String, name: String },
    Effect { kind: String, name: String },
    Sample(PathBuf),
}

impl Payload {
    pub fn icon(&self) -> Icon {
        match self {
            Payload::Instrument { .. } => Icon::Instrument,
            Payload::Effect { .. } => Icon::Effect,
            Payload::Sample(_) => Icon::Sample,
        }
    }

    pub fn name(&self) -> String {
        match self {
            Payload::Instrument { name, .. } | Payload::Effect { name, .. } => name.clone(),
            Payload::Sample(p) => p.file_stem().map_or(String::new(), |s| s.to_string_lossy().into_owned()),
        }
    }
}

/// Où l'on peut déposer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DropTarget {
    /// Le rack : un nouveau canal.
    Rack,
    Channel(usize),
    Track(usize),
    /// Emplacement d'insert précis d'une piste.
    Slot(usize, usize),
    /// Ligne de playlist, au tick indiqué.
    Lane { lane: usize, tick: u32 },
    /// L'Atelier : l'échantillon s'y ouvre.
    Lab,
}

pub struct Dnd {
    pub payload: Payload,
    x0: f32,
    y0: f32,
    /// Le geste a quitté le point de départ : c'est un glisser, plus un clic.
    pub active: bool,
}

const DND_ID: &str = "dnd";

impl App {
    /// Saisie d'un élément du navigateur.
    pub fn begin_dnd(&mut self, input: &Input, payload: Payload) {
        self.dnd = Some(Dnd { payload, x0: input.x, y0: input.y, active: false });
        self.begin_drag(wid(DND_ID, 0, 0), input, 0.0);
    }

    pub fn dnd_active(&self) -> bool {
        self.dnd.as_ref().is_some_and(|d| d.active)
    }

    fn accepts(payload: &Payload, target: DropTarget) -> bool {
        matches!(
            (payload, target),
            (Payload::Instrument { .. }, DropTarget::Rack | DropTarget::Channel(_))
                | (Payload::Effect { .. }, DropTarget::Channel(_) | DropTarget::Track(_) | DropTarget::Slot(..))
                | (Payload::Sample(_), DropTarget::Rack | DropTarget::Channel(_) | DropTarget::Lane { .. } | DropTarget::Lab)
        )
    }

    /// Déclare une zone de dépôt pendant un glisser ; renvoie vrai si elle est visée.
    pub fn drop_zone(&mut self, input: &Input, r: Rect, target: DropTarget) -> bool {
        let Some(d) = self.dnd.as_ref().filter(|d| d.active) else { return false };
        if !Self::accepts(&d.payload, target) {
            return false;
        }
        self.drops.push((r, target));
        input.inside && r.contains(input.x, input.y)
    }

    /// Fantôme sous le curseur, surlignage de la cible, dépôt au relâchement.
    pub fn dnd_layer(&mut self, c: &mut Canvas, input: &Input) {
        let drops = std::mem::take(&mut self.drops);
        let Some(d) = self.dnd.as_mut() else { return };
        if !d.active && ((input.x - d.x0).abs() > 5.0 || (input.y - d.y0).abs() > 5.0) {
            d.active = true;
        }
        let (active, payload) = (d.active, d.payload.clone());
        let t = self.theme;
        let target = drops.iter().rev().find(|(r, _)| input.inside && r.contains(input.x, input.y)).copied();
        if active {
            // Saisi au-dessus d'une zone de dépôt ; interdit ailleurs.
            self.set_cursor(if target.is_some() { Cursor::Grab } else { Cursor::NotAllowed });
            if let Some((r, _)) = target {
                c.fill_rrect(r, 10.0, t.accent.alpha(0.1));
                c.stroke_rrect(r, 10.0, 1.5, t.accent.alpha(0.9));
            }
            // Pastille qui suit le curseur.
            let name = payload.name();
            let tw = self.fonts.measure(self.f.sans_med, 13.0, &name).min(220.0);
            let chip = Rect::new(input.x + 10.0, input.y + 8.0, tw + 42.0, 32.0);
            c.fill_rrect(chip.translate(0.0, 4.0), 12.0, Color::rgba(0, 0, 0, 70));
            c.fill_rrect(chip, 10.0, t.raised_hi);
            c.stroke_rrect(chip, 10.0, 1.0, if target.is_some() { t.accent } else { t.hairline.alpha(2.0) });
            draw_icon(c, payload.icon(), chip.x + 17.0, chip.y + 16.0, 15.0, if target.is_some() { t.accent } else { t.text2 });
            self.text_fit(c, self.f.sans_med, 13.0, chip.x + 32.0, chip.y + 16.0, &name, 220.0, t.text);
        }
        if !input.released && input.down {
            return;
        }
        self.dnd = None;
        if !active {
            self.add_default(payload);
        } else if let Some((_, target)) = target {
            self.drop_payload(payload, target);
        }
    }

    /// Clic sur un élément du navigateur : l'ajout le plus probable.
    pub(super) fn add_default(&mut self, payload: Payload) {
        match payload {
            Payload::Instrument { kind, name } => self.add_channel_kind(&kind, &name),
            Payload::Effect { kind, .. } => {
                let track = self.focused_track();
                self.insert_effect(track, None, &kind);
            }
            Payload::Sample(path) => {
                let path = path.to_string_lossy().into_owned();
                match self.view {
                    View::Playlist => self.add_audio_clip(&path, None),
                    View::Lab => self.open_in_lab(&path, super::samplelab::Source::Browser),
                    _ => self.add_sample_channel(&path),
                }
            }
        }
    }

    fn drop_payload(&mut self, payload: Payload, target: DropTarget) {
        match (payload, target) {
            (Payload::Instrument { kind, name }, DropTarget::Rack) => self.add_channel_kind(&kind, &name),
            (Payload::Instrument { kind, name }, DropTarget::Channel(ch)) => {
                let c = &mut self.project.channels[ch];
                c.instrument = InstrumentDef::new(&kind);
                c.name = name.clone();
                self.focus = Focus::Channel(ch);
                self.commit(tr("Replace instrument"));
                self.toast(&tr_fmt!("{} loaded", name.as_str()));
            }
            (Payload::Effect { kind, .. }, DropTarget::Channel(ch)) => {
                let track = self.project.channels[ch].track;
                self.insert_effect(track, None, &kind);
            }
            (Payload::Effect { kind, .. }, DropTarget::Track(t)) => self.insert_effect(t, None, &kind),
            (Payload::Effect { kind, .. }, DropTarget::Slot(t, s)) => self.insert_effect(t, Some(s), &kind),
            (Payload::Sample(path), DropTarget::Rack) => self.add_sample_channel(&path.to_string_lossy()),
            (Payload::Sample(path), DropTarget::Channel(ch)) => {
                let path = path.to_string_lossy().into_owned();
                let kind = self.project.channels[ch].instrument.kind.clone();
                if super::devices::sample_kind(&kind) {
                    if self.ensure_sample(&path) {
                        self.project.channels[ch].instrument.sample = Some(path);
                        self.focus = Focus::Channel(ch);
                        self.commit(tr("Load a sample"));
                    }
                } else {
                    self.add_sample_channel(&path);
                }
            }
            (Payload::Sample(path), DropTarget::Lab) => {
                self.open_in_lab(&path.to_string_lossy(), super::samplelab::Source::Browser);
            }
            (Payload::Sample(path), DropTarget::Lane { lane, tick }) => {
                self.add_audio_clip(&path.to_string_lossy(), Some((lane, tick)));
            }
            _ => {}
        }
    }

    /// Nouveau canal d'un type d'instrument quelconque (registre ou plugin).
    pub fn add_channel_kind(&mut self, kind: &str, name: &str) {
        let idx = self.project.add_channel(name.to_string(), InstrumentDef::new(kind), 60);
        self.focus = Focus::Channel(idx);
        self.commit(tr("Add a channel"));
        self.toast(&tr_fmt!("{} added to the rack", name));
    }

    /// Pose un effet sur une piste : dans `slot` (remplacé), sinon au premier libre.
    pub fn insert_effect(&mut self, track: usize, slot: Option<usize>, kind: &str) {
        if self.project.tracks[track].kind == ponzik_project::TrackKind::Vca {
            self.toast(tr("A VCA carries no sound: put the effect on a bus"));
            return;
        }
        let inserts = &self.project.tracks[track].inserts;
        let Some(slot) = slot.filter(|&s| s < INSERT_SLOTS).or_else(|| inserts.iter().position(Option::is_none)) else {
            self.toast(tr("No free slot left on this track"));
            return;
        };
        self.project.tracks[track].inserts[slot] = Some(EffectDef::new(kind));
        if self.focused_track() != track {
            self.focus = Focus::Track(track);
        }
        self.panel = true;
        self.commit(tr("Add an effect"));
    }
}
