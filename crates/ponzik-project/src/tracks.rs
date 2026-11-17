//! Mixer tracks added, inserted, deleted and moved. The project holds a
//! variable number of tracks (the master + up to 999); every change of layout
//! goes through [`Project::remap_tracks`], which renumbers everything that
//! points at a track: outputs, sends, sidechain keys, VCAs, channels, audio
//! clips, take groups, automation lanes and MIDI links. Each track keeps its
//! identity ([`TrackDef::id`]), from which the interface tells the engine
//! where each track went ([`Project::track_layout`]).

use std::collections::HashMap;

use ponzik_engine::AutoTarget;
use ponzik_engine::mixer::{MASTER, MAX_TRACKS};

use crate::midi_map::ControlTarget;
use crate::{Project, TrackDef, default_track_name, is_default_track_name};

/// Renumbers a track reference; `None`: the track is gone.
fn remap_target(target: AutoTarget, map: &impl Fn(usize) -> Option<usize>) -> Option<AutoTarget> {
    Some(match target {
        AutoTarget::EffectParam { track, slot, id } => AutoTarget::EffectParam { track: map(track)?, slot, id },
        AutoTarget::TrackGain(t) => AutoTarget::TrackGain(map(t)?),
        AutoTarget::TrackPan(t) => AutoTarget::TrackPan(map(t)?),
        other => other,
    })
}

impl Project {
    /// Number of mixer tracks, master included.
    pub fn track_count(&self) -> usize {
        self.tracks.len()
    }

    /// True while another track fits in the mixer.
    pub fn can_add_track(&self) -> bool {
        self.tracks.len() < MAX_TRACKS
    }

    /// Index of the track whose identity is `id`.
    pub fn track_index(&self, id: u32) -> Option<usize> {
        self.tracks.iter().position(|t| t.id == id)
    }

    /// New layout of the tracks: `from[t]` is the current track that becomes
    /// track `t` (`None`: a new, empty one). The master stays first; the
    /// tracks left out are deleted, and what pointed at them falls back to the
    /// master (outputs, channels, clips) or goes (sends, keys, VCAs, lanes,
    /// MIDI links). Default names follow their new number. False (nothing
    /// changes) for a layout without the master first or over [`MAX_TRACKS`].
    pub fn remap_tracks(&mut self, from: &[Option<usize>]) -> bool {
        let n_old = self.tracks.len();
        if from.first() != Some(&Some(MASTER)) || from.len() > MAX_TRACKS || from.iter().flatten().any(|&o| o >= n_old) {
            return false;
        }
        let mut to = vec![None; n_old];
        for (new, &old) in from.iter().enumerate() {
            if let Some(old) = old {
                if to[old].is_some() {
                    return false;
                }
                to[old] = Some(new);
            }
        }
        let map = |t: usize| to.get(t).copied().flatten();
        let mut old: Vec<Option<TrackDef>> = std::mem::take(&mut self.tracks).into_iter().map(Some).collect();
        self.tracks = from
            .iter()
            .enumerate()
            .map(|(new, &was)| match was.and_then(|o| old[o].take()) {
                Some(mut t) => {
                    let o = was.unwrap_or(new);
                    if o != new && is_default_track_name(&t.name) && t.name.ends_with(&format!(" {o}")) {
                        let stem = &t.name[..t.name.len() - o.to_string().len()];
                        t.name = format!("{stem}{new}");
                    }
                    t.output = map(t.output).filter(|&x| x != new).unwrap_or(MASTER);
                    for s in &mut t.sends {
                        *s = match map(s.0) {
                            Some(d) if d != new => (d, s.1),
                            _ => (MASTER, 0.0),
                        };
                    }
                    for fx in t.inserts.iter_mut().flatten() {
                        fx.sidechain = fx.sidechain.and_then(map).filter(|&k| k != new);
                    }
                    t.vca = t.vca.and_then(map).filter(|&v| v != new && v != MASTER);
                    t
                }
                None => TrackDef::new(default_track_name(new)),
            })
            .collect();
        let home = |t: usize| map(t).unwrap_or(MASTER);
        for c in &mut self.channels {
            c.track = home(c.track);
        }
        for c in &mut self.audio_clips {
            c.track = home(c.track);
        }
        for g in &mut self.take_groups {
            g.track = home(g.track);
            for take in &mut g.takes {
                take.clip.track = home(take.clip.track);
            }
        }
        self.automation.retain_mut(|a| match remap_target(a.target, &map) {
            Some(t) => {
                a.target = t;
                true
            }
            None => false,
        });
        self.midi_map.retain_mut(|m| {
            let target = match m.target {
                ControlTarget::Param(t) => remap_target(t, &map).map(ControlTarget::Param),
                ControlTarget::Send { track, slot } => map(track).map(|track| ControlTarget::Send { track, slot }),
                other => Some(other),
            };
            target.map(|t| m.target = t).is_some()
        });
        true
    }

    /// [`Project::track_in_use`] for every track at once, in one pass (a big
    /// mixer is drawn every frame).
    pub fn tracks_in_use(&self) -> Vec<bool> {
        let n = self.tracks.len();
        let mut used: Vec<bool> = self
            .tracks
            .iter()
            .enumerate()
            .map(|(i, t)| {
                i == MASTER || t.kind != crate::TrackKind::Normal || !is_default_track_name(&t.name) || t.inserts.iter().any(Option::is_some)
            })
            .collect();
        let mut mark = |t: usize| {
            if t < n {
                used[t] = true;
            }
        };
        self.channels.iter().for_each(|c| mark(c.track));
        self.audio_clips.iter().for_each(|c| mark(c.track));
        self.take_groups.iter().for_each(|g| mark(g.track));
        for (i, t) in self.tracks.iter().enumerate() {
            if t.output != i {
                mark(t.output);
            }
            if let Some(v) = t.vca.filter(|&v| v != i) {
                mark(v);
            }
            for s in t.sends.iter().filter(|s| s.1 > 0.0 && s.0 != i) {
                mark(s.0);
            }
        }
        used
    }

    /// Appends an empty track; `None` when the mixer is full.
    pub fn add_track(&mut self) -> Option<usize> {
        let t = self.tracks.len();
        if t >= MAX_TRACKS {
            return None;
        }
        self.tracks.push(TrackDef::new(default_track_name(t)));
        Some(t)
    }

    /// Inserts an empty track at `at` (1 to the track count), the next ones
    /// moving up by one.
    pub fn insert_track(&mut self, at: usize) -> Option<usize> {
        let n = self.tracks.len();
        if at == MASTER || at > n || n >= MAX_TRACKS {
            return None;
        }
        let mut from: Vec<Option<usize>> = (0..n).map(Some).collect();
        from.insert(at, None);
        self.remap_tracks(&from).then_some(at)
    }

    /// Deletes track `t` (not the master): the tracks that went into it go
    /// where it went, the next ones move down by one.
    pub fn delete_track(&mut self, t: usize) -> bool {
        let n = self.tracks.len();
        if t == MASTER || t >= n {
            return false;
        }
        let out = self.tracks[t].output;
        let out = if out == t || out >= n { MASTER } else { out };
        for (i, tr) in self.tracks.iter_mut().enumerate() {
            if i != t && tr.output == t {
                tr.output = if out == i { MASTER } else { out };
            }
        }
        let from: Vec<Option<usize>> = (0..n).filter(|&i| i != t).map(Some).collect();
        self.remap_tracks(&from)
    }

    /// Moves track `from` to the place `to` (neither is the master), the
    /// tracks between shifting by one.
    pub fn move_track(&mut self, from: usize, to: usize) -> bool {
        let n = self.tracks.len();
        if from == MASTER || to == MASTER || from >= n || to >= n || from == to {
            return false;
        }
        let mut layout: Vec<Option<usize>> = (0..n).map(Some).collect();
        let moved = layout.remove(from);
        layout.insert(to, moved);
        self.remap_tracks(&layout)
    }

    /// First unused track for something new (a channel, a bounce), or a new
    /// one at the end.
    pub fn free_or_new_track(&mut self) -> Option<usize> {
        (1..self.tracks.len()).find(|&t| !self.track_in_use(t)).or_else(|| self.add_track())
    }

    /// Brings what points beyond the mixer back inside (a file written by
    /// hand or with more tracks than it holds): channels and clips go to the
    /// last track, outputs to the master; sends, keys, VCAs, lanes and MIDI
    /// links go.
    pub(crate) fn clamp_track_refs(&mut self) {
        let n = self.tracks.len();
        let inside = |t: usize| (t < n).then_some(t);
        for (i, t) in self.tracks.iter_mut().enumerate() {
            if t.output >= n || t.output == i {
                t.output = MASTER;
            }
            for s in &mut t.sends {
                if s.0 >= n {
                    *s = (MASTER, 0.0);
                }
            }
            for fx in t.inserts.iter_mut().flatten() {
                fx.sidechain = fx.sidechain.and_then(inside);
            }
            t.vca = t.vca.and_then(inside);
        }
        let last = n.saturating_sub(1);
        for c in &mut self.channels {
            c.track = c.track.min(last);
        }
        for c in &mut self.audio_clips {
            c.track = c.track.min(last);
        }
        for g in &mut self.take_groups {
            g.track = g.track.min(last);
            for take in &mut g.takes {
                take.clip.track = take.clip.track.min(last);
            }
        }
        self.automation.retain(|a| remap_target(a.target, &inside).is_some());
        self.midi_map.retain(|m| match m.target {
            ControlTarget::Param(t) => remap_target(t, &inside).is_some(),
            ControlTarget::Send { track, .. } => track < n,
            ControlTarget::Transport(_) => true,
        });
    }

    /// Where the tracks of `old` went in this project, by identity: `from[t]`
    /// is the track of `old` that became track `t` (`None`: a new one).
    /// `None` when nothing moved. Two projects without a track in common (one
    /// opened in place of the other) map by position.
    pub fn track_layout(&self, old: &Project) -> Option<Vec<Option<usize>>> {
        let same = |a: &[TrackDef], b: &[TrackDef]| a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.id == y.id);
        if same(&old.tracks, &self.tracks) {
            return None;
        }
        let ids: HashMap<u32, usize> = old.tracks.iter().enumerate().map(|(i, t)| (t.id, i)).collect();
        let mut from: Vec<Option<usize>> = self.tracks.iter().map(|t| ids.get(&t.id).copied()).collect();
        if from.iter().all(Option::is_none) {
            from = (0..self.tracks.len()).map(|t| (t < old.tracks.len()).then_some(t)).collect();
            if from.len() == old.tracks.len() {
                return None;
            }
        }
        Some(from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::midi_map::{CtlMode, CtlSource, MidiMapping};
    use crate::{AudioClipDef, AutoScope, EffectDef, InstrumentDef, TakeGroup, TrackKind};

    /// A project whose references point at tracks 3 to 6.
    fn wired() -> Project {
        let mut p = Project::empty();
        p.tracks[3].output = 5;
        p.tracks[3].sends[0] = (6, 0.5);
        p.tracks[4].vca = Some(6);
        p.tracks[6].kind = TrackKind::Vca;
        let mut comp = EffectDef::new("compressor");
        comp.sidechain = Some(4);
        p.tracks[5].inserts[1] = Some(comp);
        p.tracks[5].name = "Drums".into();
        let c = p.add_channel("Kick", InstrumentDef::new("drumsynth"), 36);
        p.channels[c].track = 4;
        p.audio_clips.push(AudioClipDef { path: "a.wav".into(), track: 5, ..Default::default() });
        let take = crate::Take { name: "Take 1".into(), clip: AudioClipDef { path: "t.wav".into(), track: 6, ..Default::default() } };
        p.take_groups.push(TakeGroup::new(0, 6, vec![take]));
        p.add_automation_point(AutoTarget::TrackGain(4), AutoScope::Song, 0, 0.5);
        p.add_automation_point(AutoTarget::EffectParam { track: 5, slot: 1, id: 0 }, AutoScope::Song, 0, -20.0);
        p.add_automation_point(AutoTarget::ChannelGain(0), AutoScope::Song, 0, 0.5);
        let cc = CtlSource::Cc { channel: 0, cc: 1 };
        p.midi_map.push(MidiMapping::new(cc, ControlTarget::Param(AutoTarget::TrackPan(3)), CtlMode::Absolute));
        p.midi_map.push(MidiMapping::new(cc, ControlTarget::Send { track: 4, slot: 0 }, CtlMode::Absolute));
        p
    }

    fn lane_targets(p: &Project) -> Vec<AutoTarget> {
        p.automation.iter().map(|a| a.target).collect()
    }

    #[test]
    fn inserting_a_track_renumbers_everything_after_it() {
        let mut p = wired();
        let ids: Vec<u32> = p.tracks.iter().map(|t| t.id).collect();
        assert_eq!(p.insert_track(2), Some(2));
        assert_eq!(p.track_count(), 34);
        assert_eq!(p.tracks[2], TrackDef::new("Track 2"));
        assert_eq!((p.tracks[4].output, p.tracks[4].sends[0]), (6, (7, 0.5)));
        assert_eq!(p.tracks[5].vca, Some(7));
        assert_eq!(p.tracks[6].inserts[1].as_ref().unwrap().sidechain, Some(5));
        assert_eq!((p.tracks[6].name.as_str(), p.tracks[4].name.as_str()), ("Drums", "Track 4"), "default names follow");
        assert_eq!(p.channels[0].track, 5);
        assert_eq!(p.audio_clips[0].track, 6);
        assert_eq!((p.take_groups[0].track, p.take_groups[0].takes[0].clip.track), (7, 7));
        assert_eq!(
            lane_targets(&p),
            [AutoTarget::TrackGain(5), AutoTarget::EffectParam { track: 6, slot: 1, id: 0 }, AutoTarget::ChannelGain(0)]
        );
        assert_eq!(p.midi_map[0].target, ControlTarget::Param(AutoTarget::TrackPan(4)));
        assert_eq!(p.midi_map[1].target, ControlTarget::Send { track: 5, slot: 0 });
        assert_eq!(p.tracks[7].id, ids[6], "the identity follows the track");
        assert!(p.routing_order().is_some());
        // Master first, or nothing.
        assert_eq!(p.insert_track(0), None);
        assert_eq!(p.insert_track(p.track_count() + 1), None);
    }

    #[test]
    fn deleting_a_track_drops_the_links_to_it() {
        let mut p = wired();
        // Track 5 (a bus of track 3, sidechained by 4) goes: 3 goes where it went.
        p.tracks[5].output = 2;
        assert!(p.delete_track(5));
        assert_eq!(p.track_count(), 32);
        assert_eq!(p.tracks[3].output, 2, "its members go where it went");
        assert_eq!(p.tracks[3].sends[0], (5, 0.5), "track 6 is now 5");
        assert_eq!(p.tracks[4].vca, Some(5));
        assert_eq!(p.audio_clips[0].track, MASTER, "its clips go to the master");
        assert_eq!(p.take_groups[0].track, 5);
        assert_eq!(lane_targets(&p), [AutoTarget::TrackGain(4), AutoTarget::ChannelGain(0)], "its lanes go");
        // The VCA goes in turn: the tracks it drove are free.
        assert!(p.delete_track(5));
        assert_eq!(p.tracks[4].vca, None);
        assert_eq!(p.tracks[3].sends[0], (MASTER, 0.0));
        assert_eq!(p.take_groups[0].track, MASTER);
        assert_eq!(p.midi_map.len(), 2);
        assert!(p.delete_track(4));
        assert_eq!(p.midi_map.len(), 1, "the link to the send of track 4 goes");
        assert_eq!(p.channels[0].track, MASTER);
        assert!(!p.delete_track(MASTER) && !p.delete_track(99));
    }

    #[test]
    fn moving_a_track_keeps_its_links() {
        let mut p = wired();
        // Track 6 (the VCA) to place 1: 1..=5 move up by one.
        assert!(p.move_track(6, 1));
        assert_eq!(p.tracks[1].kind, TrackKind::Vca);
        assert_eq!((p.tracks[4].output, p.tracks[4].sends[0]), (6, (1, 0.5)));
        assert_eq!(p.tracks[5].vca, Some(1));
        assert_eq!(p.tracks[6].inserts[1].as_ref().unwrap().sidechain, Some(5));
        assert_eq!(p.channels[0].track, 5);
        assert_eq!(p.take_groups[0].track, 1);
        // And back: the project is as it was.
        assert!(p.move_track(1, 6));
        assert_eq!(p, wired());
        assert!(!p.move_track(0, 3) && !p.move_track(3, 0) && !p.move_track(3, 3));
    }

    #[test]
    fn layouts_follow_track_identities() {
        let a = wired();
        assert_eq!(a.track_layout(&a), None);
        let mut b = a.clone();
        b.insert_track(2);
        b.delete_track(10);
        b.move_track(3, 30);
        let from = b.track_layout(&a).unwrap();
        assert_eq!(from.len(), b.track_count());
        for (t, f) in from.iter().enumerate() {
            match f {
                Some(o) => assert_eq!(a.tracks[*o].id, b.tracks[t].id),
                None => assert_eq!(t, 2),
            }
        }
        assert!(!from.contains(&Some(9)), "the deleted track is gone");
        // An appended track.
        let mut c = a.clone();
        c.add_track();
        let from = c.track_layout(&a).unwrap();
        assert_eq!((from.len(), from[33]), (34, None));
        assert!(from[..33].iter().enumerate().all(|(t, f)| *f == Some(t)));
        // Another project: by position.
        let mut d = Project::empty();
        assert_eq!(d.track_layout(&a), None);
        d.add_track();
        assert_eq!(d.track_layout(&a).unwrap()[33], None);
    }

    #[test]
    fn tracks_in_use_match_one_by_one() {
        let mut p = wired();
        p.tracks[9].sends[1] = (12, 0.4);
        p.tracks[14].vca = Some(15);
        p.add_track();
        let one_by_one: Vec<bool> = (0..p.track_count()).map(|t| p.track_in_use(t)).collect();
        assert_eq!(p.tracks_in_use(), one_by_one);
        assert!(one_by_one[12] && one_by_one[15] && !one_by_one[33]);
    }

    #[test]
    fn tracks_grow_up_to_the_limit() {
        let mut p = Project::empty();
        while p.add_track().is_some() {}
        assert_eq!(p.track_count(), MAX_TRACKS);
        assert_eq!(p.insert_track(5), None);
        assert!(p.delete_track(5) && p.insert_track(5) == Some(5));
        assert_eq!(p.tracks[MAX_TRACKS - 1].name, format!("Track {}", MAX_TRACKS - 1));
    }
}
