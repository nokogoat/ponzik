//! Glisser-déposer depuis le navigateur : un instrument vers le rack, un effet
//! vers une piste ou un emplacement d'insert, un échantillon vers la playlist ou
//! un sampler. Un simple clic garde le comportement d'ajout direct.
//!
//! Files dragged from the file manager go through the same zones: WAV files
//! become sampler channels, audio clips, pads or open in the Sample Lab, MIDI
//! files become patterns, a project opens.

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
    /// Files from another application (unknown until dropped on X11).
    Files(Vec<PathBuf>),
}

impl Payload {
    pub fn icon(&self) -> Icon {
        match self {
            Payload::Instrument { .. } => Icon::Instrument,
            Payload::Effect { .. } => Icon::Effect,
            Payload::Sample(_) => Icon::Sample,
            Payload::Files(_) => Icon::Folder,
        }
    }

    pub fn name(&self) -> String {
        match self {
            Payload::Instrument { name, .. } | Payload::Effect { name, .. } => name.clone(),
            Payload::Sample(p) => p.file_stem().map_or(String::new(), |s| s.to_string_lossy().into_owned()),
            Payload::Files(files) => match files.as_slice() {
                [] => tr("Files").to_string(),
                [one] => one.file_name().map_or(String::new(), |s| s.to_string_lossy().into_owned()),
                many => tr_fmt!("{} files", many.len()),
            },
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
    /// A Drum Pads pad; `add_layer` (Shift held) adds a velocity layer.
    Pad { channel: usize, pad: usize, add_layer: bool },
    /// The end of one chain of a rack.
    RackChain { track: usize, slot: usize, chain: usize },
    /// A slot of the Session view: an audio clip.
    SessionSlot { col: usize, row: usize },
}

pub struct Dnd {
    pub payload: Payload,
    x0: f32,
    y0: f32,
    /// Le geste a quitté le point de départ : c'est un glisser, plus un clic.
    pub active: bool,
    /// Files dragged from another application: dropped by the system, not
    /// by a mouse release.
    external: bool,
}

/// What a dropped file is, by its extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    Audio,
    /// Audio in a format not read yet (FLAC, AIFF, MP3, OGG).
    OtherAudio,
    Midi,
    Project,
    FlStudio,
    Unknown,
}

pub fn file_kind(path: &std::path::Path) -> FileKind {
    let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "wav" | "wave" => FileKind::Audio,
        "flac" | "aif" | "aiff" | "mp3" | "ogg" | "opus" | "m4a" => FileKind::OtherAudio,
        "mid" | "midi" | "smf" => FileKind::Midi,
        "ponzik" => FileKind::Project,
        "flp" => FileKind::FlStudio,
        _ => FileKind::Unknown,
    }
}

const DND_ID: &str = "dnd";

impl App {
    /// Saisie d'un élément du navigateur.
    pub fn begin_dnd(&mut self, input: &Input, payload: Payload) {
        self.dnd = Some(Dnd { payload, x0: input.x, y0: input.y, active: false, external: false });
        self.begin_drag(wid(DND_ID, 0, 0), input, 0.0);
    }

    pub fn dnd_active(&self) -> bool {
        self.dnd.as_ref().is_some_and(|d| d.active)
    }

    fn accepts(payload: &Payload, target: DropTarget) -> bool {
        matches!(
            (payload, target),
            (Payload::Instrument { .. }, DropTarget::Rack | DropTarget::Channel(_))
                | (Payload::Effect { .. }, DropTarget::Channel(_) | DropTarget::Track(_) | DropTarget::Slot(..) | DropTarget::RackChain { .. })
                | (
                    Payload::Sample(_) | Payload::Files(_),
                    DropTarget::Rack
                        | DropTarget::Channel(_)
                        | DropTarget::Lane { .. }
                        | DropTarget::Lab
                        | DropTarget::Pad { .. }
                        | DropTarget::SessionSlot { .. }
                )
        )
    }

    /// Files dragged from another application over the window: a drag of
    /// their own, so that the views offer their drop zones.
    pub fn os_drag(&mut self, input: &Input) {
        if !input.file_hover && input.dropped.is_none() {
            return;
        }
        let paths = input.dropped.clone().unwrap_or_default();
        match self.dnd.as_mut() {
            Some(d) if d.external => {
                if !paths.is_empty() {
                    d.payload = Payload::Files(paths);
                }
            }
            _ => {
                self.stop_preview();
                self.dnd = Some(Dnd { payload: Payload::Files(paths), x0: input.x, y0: input.y, active: true, external: true });
            }
        }
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
        let (active, external, payload) = (d.active, d.external, d.payload.clone());
        let t = self.theme;
        let target = drops.iter().rev().find(|(r, _)| input.inside && r.contains(input.x, input.y)).copied();
        if external {
            // The whole window takes files; the zone under the pointer says where they go.
            let (w, h) = c.logical_size();
            let pulse = 0.55 + 0.25 * (self.time * 4.0).sin();
            self.anim.set(wid("dnd-os", 0, 0), self.time);
            c.stroke_rrect(Rect::new(3.0, 3.0, w - 6.0, h - 6.0), 12.0, 2.0, t.accent.alpha(if target.is_some() { 0.35 } else { pulse }));
        }
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
        if external {
            let Some(paths) = input.dropped.clone() else {
                if !input.file_hover {
                    self.dnd = None;
                }
                return;
            };
            self.dnd = None;
            self.drop_files(paths, target.map(|t| t.1));
            return;
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
        self.note_recent(&payload);
        match payload {
            Payload::Files(paths) => self.drop_files(paths, None),
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
        self.note_recent(&payload);
        match (payload, target) {
            (Payload::Files(paths), target) => self.drop_files(paths, Some(target)),
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
            (Payload::Effect { kind, .. }, DropTarget::RackChain { track, slot, chain }) => {
                if ponzik_project::rack::rack_accepts(&kind) {
                    self.run_rack_action(super::rackfx::RackAction::Add { track, slot, chain, kind });
                } else {
                    self.toast(tr("Only built-in effects go in a rack"));
                }
            }
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
            (Payload::Sample(path), DropTarget::Pad { channel, pad, add_layer }) => {
                self.load_pad_sample(channel, pad, &path.to_string_lossy(), add_layer);
            }
            (Payload::Sample(path), DropTarget::Lab) => {
                self.open_in_lab(&path.to_string_lossy(), super::samplelab::Source::Browser);
            }
            (Payload::Sample(path), DropTarget::Lane { lane, tick }) => {
                self.add_audio_clip(&path.to_string_lossy(), Some((lane, tick)));
            }
            (Payload::Sample(path), DropTarget::SessionSlot { col, row }) => {
                self.add_session_audio(&path.to_string_lossy(), col, row);
            }
            _ => {}
        }
    }

    /// Files dropped from another application on `target` (or anywhere else).
    pub fn drop_files(&mut self, paths: Vec<PathBuf>, target: Option<DropTarget>) {
        let of = |kind: FileKind| paths.iter().filter(|p| file_kind(p) == kind).cloned().collect::<Vec<_>>();
        // A project replaces everything: it goes first and alone.
        if let Some(p) = paths.iter().find(|p| matches!(file_kind(p), FileKind::Project | FileKind::FlStudio)) {
            self.open_file(p.clone());
            return;
        }
        let (audio, midi) = (of(FileKind::Audio), of(FileKind::Midi));
        let other = paths.iter().filter(|p| file_kind(p) == FileKind::OtherAudio).count();
        if audio.is_empty() && midi.is_empty() {
            self.toast(if other > 0 { tr("Only WAV files can be read for now") } else { tr("Ponzik can't use these files") });
            return;
        }
        for (i, path) in audio.iter().enumerate() {
            // Known to the browser from now on.
            if !self.sample_files.contains(path) {
                self.sample_files.push(path.clone());
            }
            self.note_recent(&Payload::Sample(path.clone()));
            let p = path.to_string_lossy().into_owned();
            match target {
                Some(DropTarget::Lane { lane, tick }) => self.add_audio_clip(&p, Some((lane + i, tick))),
                Some(DropTarget::Pad { channel, pad, add_layer }) => {
                    self.load_pad_sample(channel, pad + if add_layer { 0 } else { i }, &p, add_layer)
                }
                Some(DropTarget::Lab) if i == 0 => self.open_in_lab(&p, super::samplelab::Source::Browser),
                // Several files fill the slots below.
                Some(DropTarget::SessionSlot { col, row }) if row + i < ponzik_project::SESSION_SCENES => self.add_session_audio(&p, col, row + i),
                Some(DropTarget::Channel(ch)) if i == 0 => self.drop_payload(Payload::Sample(path.clone()), DropTarget::Channel(ch)),
                Some(DropTarget::Lab | DropTarget::Channel(_) | DropTarget::Rack) => self.add_sample_channel(&p),
                _ => match self.view {
                    View::Playlist => self.add_audio_clip(&p, None),
                    View::Lab if i == 0 => self.open_in_lab(&p, super::samplelab::Source::Browser),
                    _ => self.add_sample_channel(&p),
                },
            }
        }
        for (i, path) in midi.iter().enumerate() {
            match super::compat::import_midi(&mut self.project, path) {
                Ok(p) => {
                    let name = self.project.patterns[p].name.clone();
                    if let Some(DropTarget::Lane { lane, tick }) = target {
                        let len = self.project.patterns[p].len;
                        self.project.clips.push(ponzik_project::ClipDef { pattern: p, lane: lane + audio.len() + i, start: tick, len, offset: 0, muted: false });
                        self.commit(tr("Import a MIDI file"));
                        self.toast(&tr_fmt!("\"{}\" placed in the playlist", name.as_str()));
                    } else {
                        self.select_pattern(p);
                        self.commit(tr("Import a MIDI file"));
                        self.toast(&tr_fmt!("Imported into the pattern \"{}\"", name.as_str()));
                    }
                }
                Err(e) => self.toast(&e),
            }
        }
        if other > 0 {
            self.toast(tr("Only WAV files can be read for now"));
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn dropped_files_are_told_by_their_extension() {
        assert_eq!(file_kind(Path::new("/a/Kick 01.WAV")), FileKind::Audio);
        assert_eq!(file_kind(Path::new("/a/loop.flac")), FileKind::OtherAudio);
        assert_eq!(file_kind(Path::new("/a/riff.MID")), FileKind::Midi);
        assert_eq!(file_kind(Path::new("/a/song.ponzik")), FileKind::Project);
        assert_eq!(file_kind(Path::new("/a/song.flp")), FileKind::FlStudio);
        assert_eq!(file_kind(Path::new("/a/notes.txt")), FileKind::Unknown);
        assert_eq!(file_kind(Path::new("/a/no-extension")), FileKind::Unknown);
    }

    #[test]
    fn files_go_where_samples_go() {
        let files = Payload::Files(Vec::new());
        for target in [DropTarget::Rack, DropTarget::Channel(0), DropTarget::Lab, DropTarget::Lane { lane: 0, tick: 0 }] {
            assert!(App::accepts(&files, target), "{target:?}");
        }
        assert!(App::accepts(&files, DropTarget::Pad { channel: 0, pad: 0, add_layer: false }));
        assert!(!App::accepts(&files, DropTarget::Track(1)));
        assert!(!App::accepts(&files, DropTarget::Slot(1, 0)));
        assert_eq!(files.name(), tr("Files"));
        assert_eq!(Payload::Files(vec!["/a/b.wav".into()]).name(), "b.wav");
        assert_eq!(Payload::Files(vec!["/a".into(), "/b".into()]).name(), tr_fmt!("{} files", 2));
    }
}
