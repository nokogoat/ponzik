//! Mixer tracks added, inserted, deleted and moved from the interface, and
//! what follows a new track layout: focus, selection, solo, armed tracks,
//! meters, open devices. The layout itself lives in the project
//! (`Project::remap_tracks`); undo and redo go through the same path.

use ponzik_engine::mixer::MASTER;
use ponzik_i18n::{tr, tr_fmt};

use super::app::{App, Focus, Meter};
use super::devices::Device;
use super::widgets::wid;

/// A strip dragged by its name.
#[derive(Clone, Copy, Debug)]
pub struct TrackDrag {
    pub from: usize,
    /// Pointer x when the name was pressed.
    pub x0: f32,
    /// Moved far enough to be a drag rather than a click.
    pub active: bool,
    /// Where it would land: the track index it takes.
    pub to: Option<usize>,
    /// Held without a button (captures).
    pub pinned: bool,
}

impl App {
    /// The interface follows a new track layout: `from[t]` is the track that
    /// became track `t` (`None`: a new one).
    pub(super) fn follow_track_layout(&mut self, from: &[Option<usize>]) {
        let n_old = self.synced.tracks.len();
        let mut to = vec![None; n_old];
        for (t, w) in from.iter().enumerate() {
            if let Some(o) = w.filter(|&o| o < n_old) {
                to[o] = Some(t);
            }
        }
        let map = |t: usize| to.get(t).copied().flatten();
        // An index past the old tracks was set for the new layout (a track just made).
        if let Focus::Track(t) = self.focus
            && t < n_old
        {
            self.focus = Focus::Track(map(t).unwrap_or(MASTER));
        }
        self.mixer_sel = self.mixer_sel.iter().filter_map(|&t| map(t)).collect();
        self.solo = self.solo.and_then(map);
        self.rec.follow_tracks(map);
        self.rack_sel = std::mem::take(&mut self.rack_sel).into_iter().filter_map(|((t, s), u)| Some(((map(t)?, s), u))).collect();
        let device = |d: Device| match d {
            Device::Effect { track, slot } => map(track).map(|track| Device::Effect { track, slot }),
            Device::RackUnit { track, slot, unit } => map(track).map(|track| Device::RackUnit { track, slot, unit }),
            d => Some(d),
        };
        self.layout.wins.retain_mut(|w| match device(w.device) {
            Some(d) => {
                w.device = d;
                true
            }
            None => false,
        });
        let old = std::mem::take(&mut self.track_meters);
        self.track_meters = from.iter().map(|w| w.and_then(|o| old.get(o).copied()).unwrap_or_default()).collect();
        // Strip animations are kept by place: new tracks slide in.
        for (t, w) in from.iter().enumerate() {
            if w.is_none() {
                self.anim.set(wid("mx-vis", t, 0), 0.0);
                self.anim.set(wid("mx-born", t, 0), 1.0);
            }
        }
        if let Some(d) = self.track_drag.as_mut() {
            match map(d.from) {
                Some(f) => d.from = f,
                None => self.track_drag = None,
            }
        }
    }

    /// Shows a track made by hand, focuses it and scrolls to it.
    fn show_new_track(&mut self, t: usize) {
        let id = self.project.tracks[t].id;
        if !self.mixer_shown.contains(&id) {
            self.mixer_shown.push(id);
        }
        self.focus = Focus::Track(t);
        self.mixer_sel.clear();
        self.mixer_reveal = Some(t);
    }

    fn mixer_full(&mut self) {
        self.toast(&tr_fmt!("The mixer is full ({} tracks)", ponzik_engine::mixer::MAX_TRACKS - 1));
    }

    /// "Add track": an empty track at the end of the mixer.
    pub fn add_mixer_track(&mut self) -> Option<usize> {
        let Some(t) = self.project.add_track() else {
            self.mixer_full();
            return None;
        };
        self.commit(tr("Add track"));
        self.show_new_track(t);
        Some(t)
    }

    /// "Insert track": an empty track at `at`, the next ones moving right.
    pub fn insert_mixer_track(&mut self, at: usize) {
        if !self.project.can_add_track() {
            return self.mixer_full();
        }
        let Some(t) = self.project.insert_track(at) else { return };
        self.commit(tr("Insert track"));
        self.show_new_track(t);
    }

    /// "Delete track": what went into it goes where it went; Ctrl+Z brings it back.
    pub fn delete_mixer_track(&mut self, t: usize) {
        if t == MASTER || t >= self.project.tracks.len() {
            return;
        }
        let name = self.project.tracks[t].name.clone();
        if self.project.delete_track(t) {
            self.commit(tr("Delete track"));
            self.toast(&tr_fmt!("{} removed · Ctrl+Z to undo", name));
        }
    }

    /// Moves track `from` to the place `to`, its links and channels with it.
    pub fn move_mixer_track(&mut self, from: usize, to: usize) {
        if self.project.move_track(from, to) {
            self.commit(tr("Move a track"));
            self.mixer_reveal = Some(to);
        }
    }

    /// "Route to new track": the channel gets a track of its own, at the end.
    pub fn route_channel_to_new_track(&mut self, ch: usize) {
        let Some(name) = self.project.channels.get(ch).map(|c| c.name.clone()) else { return };
        let Some(t) = self.project.add_track() else { return self.mixer_full() };
        self.project.tracks[t].name = name.clone();
        self.project.channels[ch].track = t;
        self.commit(tr("Route to new track"));
        self.show_new_track(t);
        self.focus = Focus::Channel(ch);
        self.toast(&tr_fmt!("{} plays on track {}", name, t));
    }

    /// Captures: `PONZIK_TRACKS=64[,compact][,end][,drag:from>to][,menu:t]` grows
    /// the demo to that many tracks, the channels spread over them, every track shown.
    pub fn screenshot_tracks(&mut self, spec: &str) {
        let mut parts = spec.split(',').map(str::trim);
        let count = parts.next().and_then(|n| n.parse::<usize>().ok()).unwrap_or(64);
        while self.project.tracks.len() < count {
            self.project.add_track();
        }
        let n = self.project.channels.len();
        for c in 0..n {
            let t = 1 + c * (count - 2) / n.max(1);
            let old = self.project.channels[c].track;
            if old != t && t < count {
                self.project.tracks[old].name = ponzik_project::default_track_name(old);
                self.project.channels[c].track = t;
                self.project.tracks[t].name = self.project.channels[c].name.clone();
            }
        }
        for t in (36..count).step_by(9) {
            self.project.tracks[t].name = format!("Stem {t}");
            self.project.tracks[t].inserts[0] = Some(ponzik_project::EffectDef::new("eq"));
        }
        self.commit(tr("Add track"));
        self.anim.set(wid("mx-all", 0, 0), 1.0);
        for part in parts {
            let (name, arg) = part.split_once(':').unwrap_or((part, ""));
            match name {
                "compact" => self.mixer_compact = true,
                "end" => self.mixer_reveal = Some(self.project.tracks.len() - 1),
                "drag" => {
                    let (a, b) = arg.split_once('>').unwrap_or((arg, arg));
                    if let (Ok(from), Ok(to)) = (a.parse(), b.parse()) {
                        self.track_drag = Some(TrackDrag { from, x0: 0.0, active: true, to: Some(to), pinned: true });
                    }
                }
                "menu" => {
                    if let Ok(t) = arg.parse::<usize>() {
                        self.focus = Focus::Track(t);
                        let at = ponzik_ui::Rect::new(520.0, 200.0, 0.0, 0.0);
                        self.open_popup(super::app::Popup::TrackMenu(t), at);
                    }
                }
                _ => {}
            }
        }
        for t in 0..self.project.tracks.len() {
            self.anim.set(wid("mx-born", t, 0), 0.0);
            self.anim.set(wid("mx-vis", t, 0), 1.0);
        }
        self.track_meters.resize(self.project.tracks.len(), Meter::default());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::Input;
    use crate::gui::app::View;
    use ponzik_ui::Canvas;

    fn app() -> App {
        let project = ponzik_project::demo_project();
        let none = |_: &str| None;
        let (_engine, handle) = project.build_engine(48_000.0, &none);
        let mut app = App::new(handle, project, 48_000);
        app.set_view(View::Mixer);
        app
    }

    #[test]
    fn interface_state_follows_inserted_deleted_and_moved_tracks() {
        let mut app = app();
        let kick = app.project.channels[0].track;
        app.focus = Focus::Track(kick);
        app.mixer_sel = vec![kick, kick + 2];
        app.solo = Some(kick + 2);
        app.rec.armed.push((kick + 1, ponzik_engine::InputSource::Left));
        app.insert_mixer_track(kick);
        assert_eq!(app.project.channels[0].track, kick + 1);
        assert_eq!(app.focus, Focus::Track(kick), "the new track has the focus");
        assert_eq!(app.mixer_sel, [] as [usize; 0]);
        assert_eq!(app.solo, Some(kick + 3));
        assert!(app.rec.is_armed(kick + 2));
        app.undo();
        assert_eq!(app.project.channels[0].track, kick);
        assert_eq!(app.solo, Some(kick + 2));
        assert!(app.rec.is_armed(kick + 1));
        // Deleting the soloed track ends the solo.
        app.delete_mixer_track(kick + 2);
        assert_eq!(app.solo, None);
        app.redo();
        app.undo();
        // Moving: the focus goes with the track.
        app.focus = Focus::Track(kick);
        app.move_mixer_track(kick, 20);
        assert_eq!(app.focus, Focus::Track(20));
        assert_eq!(app.project.channels[0].track, 20);
    }

    #[test]
    fn add_track_strip_and_route_to_new_track() {
        let mut app = app();
        let mut c = Canvas::new(1600, 960);
        let idle = Input::default();
        for _ in 0..10 {
            app.frame(&mut c, &idle, 1.0 / 60.0);
        }
        let n = app.project.track_count();
        assert_eq!(app.add_mixer_track(), Some(n));
        assert!(app.mixer_layout(false).iter().any(|s| s.tr == n), "a track added by hand shows while empty");
        app.route_channel_to_new_track(1);
        assert_eq!(app.project.channels[1].track, n + 1);
        assert_eq!(app.project.tracks[n + 1].name, app.project.channels[1].name);
        for _ in 0..10 {
            app.frame(&mut c, &idle, 1.0 / 60.0);
        }
        assert_eq!(app.track_meters.len(), n + 2);
        app.undo();
        app.undo();
        assert_eq!(app.project.track_count(), n);
    }
}
