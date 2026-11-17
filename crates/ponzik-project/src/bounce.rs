//! Parts of the project that become audio: freeze (a channel or a mixer
//! track plays its render instead of its devices), bounce in place (a
//! channel or clips rendered to an audio clip), consolidate (clips merged into
//! one audio clip) and render selection (a time range to a new audio track).
//!
//! Rendering ([`render_freeze`], [`render_bounce`]) and placing the result
//! ([`Project::place_bounce`]) are separate: the interface writes the file in
//! between, wherever the project keeps its audio.

use std::sync::Arc;

use ponzik_engine::mixer::MASTER;
use ponzik_engine::{Sample, Tap};
use ponzik_i18n::{tr, tr_fmt};

use crate::render::{RenderMode, render_taps};
use crate::{AudioClipDef, ClipDef, Project, SampleLoader, TrackDef, TrackKind};

/// What a freeze replaces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FreezeTarget {
    /// A channel's instrument and note effects (its volume and pan stay live).
    Channel(usize),
    /// A mixer track's input and inserts (its fader, pan and sends stay live).
    Track(usize),
}

/// What a bounce renders.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BounceSource {
    /// A channel over the whole song.
    Channel(usize),
    /// Playlist clips over their extent: pattern clips (every channel of
    /// their pattern) and audio clips (indices in `clips` and `audio_clips`).
    Clips { patterns: Vec<usize>, audio: Vec<usize> },
    /// Everything the song plays from `.0` to `.1` (ticks).
    Range(u32, u32),
}

/// What becomes of the source once bounced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceAction {
    Keep,
    /// Kept in the project, silent (a channel or clips muted).
    Mute,
    /// Removed (the channel, the clips, the content of the range).
    Replace,
}

/// A bounce rendered, not placed yet.
#[derive(Clone, Debug, PartialEq)]
pub struct BounceRender {
    /// Interleaved stereo from `start`, the tail included.
    pub audio: Vec<f32>,
    /// Range of the source, in ticks.
    pub start: u32,
    pub end: u32,
    /// Mixer track the clip plays on: the render is what reaches that track's
    /// input, so the clip replaces its source exactly. `None`: a new track,
    /// straight to the master (the render is what the source adds to the mix).
    pub track: Option<usize>,
}

impl Project {
    // --- Freeze ---

    /// The channel's own freeze file, when it loads.
    pub fn frozen_channel(&self, idx: usize, loader: SampleLoader) -> Option<Arc<Sample>> {
        self.channels.get(idx)?.frozen.as_deref().and_then(loader)
    }

    /// The track's freeze file, when it loads (never for the master).
    pub fn frozen_track(&self, track: usize, loader: SampleLoader) -> Option<Arc<Sample>> {
        if track == MASTER {
            return None;
        }
        self.tracks.get(track)?.frozen.as_deref().and_then(loader)
    }

    /// The channel's devices play: neither it nor its track is frozen (or
    /// their freeze file is missing, and the live devices take over).
    pub fn channel_live(&self, idx: usize, loader: SampleLoader) -> bool {
        self.frozen_channel(idx, loader).is_none()
            && self.channels.get(idx).is_none_or(|c| self.frozen_track(c.track, loader).is_none())
    }

    pub fn is_frozen(&self, target: FreezeTarget) -> bool {
        match target {
            FreezeTarget::Channel(c) => self.channels.get(c).is_some_and(|c| c.frozen.is_some()),
            FreezeTarget::Track(t) => t != MASTER && self.tracks.get(t).is_some_and(|t| t.frozen.is_some()),
        }
    }

    /// The channel's devices are read-only: it is frozen, by itself or with its track.
    pub fn channel_locked(&self, idx: usize) -> bool {
        self.is_frozen(FreezeTarget::Channel(idx))
            || self.channels.get(idx).is_some_and(|c| self.is_frozen(FreezeTarget::Track(c.track)))
    }

    /// A track can be frozen: not the master, not a VCA (no sound goes through it).
    pub fn can_freeze_track(&self, track: usize) -> bool {
        track != MASTER && self.tracks.get(track).is_some_and(|t| t.kind != TrackKind::Vca)
    }

    /// Freezes (`Some(file)`) or thaws (`None`); returns the previous file.
    pub fn set_frozen(&mut self, target: FreezeTarget, path: Option<String>) -> Option<String> {
        match target {
            FreezeTarget::Channel(c) => std::mem::replace(&mut self.channels.get_mut(c)?.frozen, path),
            FreezeTarget::Track(t) if self.can_freeze_track(t) => std::mem::replace(&mut self.tracks[t].frozen, path),
            FreezeTarget::Track(_) => None,
        }
    }

    /// Every freeze file of the project.
    pub fn freeze_paths(&self) -> impl Iterator<Item = &String> {
        self.channels.iter().filter_map(|c| c.frozen.as_ref()).chain(self.tracks.iter().filter_map(|t| t.frozen.as_ref()))
    }

    /// Name of what is frozen or bounced, for its file and its track.
    pub fn target_name(&self, target: FreezeTarget) -> String {
        match target {
            FreezeTarget::Channel(c) => self.channels.get(c).map_or_else(String::new, |c| c.name.clone()),
            FreezeTarget::Track(t) => self.tracks.get(t).map_or_else(String::new, |t| t.name.clone()),
        }
    }

    // --- Bounce ---

    /// Name of a bounce: the channel, the first clip, or "Selection".
    pub fn bounce_name(&self, source: &BounceSource) -> String {
        match source {
            BounceSource::Channel(c) => self.target_name(FreezeTarget::Channel(*c)),
            BounceSource::Clips { patterns, audio } => {
                let pattern = patterns.first().and_then(|&i| self.clips.get(i)).and_then(|c| self.patterns.get(c.pattern));
                let file = audio.first().and_then(|&i| self.audio_clips.get(i)).map(|c| {
                    let p = std::path::Path::new(&c.path);
                    p.file_stem().map_or_else(|| c.path.clone(), |s| s.to_string_lossy().into_owned())
                });
                pattern.map(|p| p.name.clone()).or(file).unwrap_or_else(|| tr("Clips").into())
            }
            BounceSource::Range(..) => tr("Selection").into(),
        }
    }

    /// Range of a source, in ticks.
    pub fn bounce_range(&self, source: &BounceSource) -> (u32, u32) {
        match source {
            BounceSource::Channel(_) => (0, self.song_len()),
            BounceSource::Clips { patterns, audio } => {
                let spans = patterns
                    .iter()
                    .filter_map(|&i| self.clips.get(i).map(|c| (c.start, c.start + c.len)))
                    .chain(audio.iter().filter_map(|&i| self.audio_clips.get(i).map(|c| (c.start, c.start + c.len))));
                spans.fold(None, |acc: Option<(u32, u32)>, (a, b)| Some(acc.map_or((a, b), |(x, y)| (x.min(a), y.max(b)))))
                    .unwrap_or((0, 0))
            }
            BounceSource::Range(a, b) => (*a, (*b).max(a + 1)),
        }
    }

    /// Places a rendered bounce, saved as `path` (`secs` long), and deals
    /// with its source. The clip goes on `render.track` (`None`: a new mixer
    /// track named after the source, routed to the master) and on a new lane,
    /// or on the lane of the first clip when `consolidate`. `whole_file`: the
    /// clip runs to the end of the file (tail included), otherwise it covers
    /// the source range. Returns the clip's index, `None` when no mixer track
    /// is free.
    #[allow(clippy::too_many_arguments)]
    pub fn place_bounce(
        &mut self,
        source: &BounceSource,
        track: Option<usize>,
        (start, end): (u32, u32),
        secs: f64,
        path: String,
        action: SourceAction,
        whole_file: bool,
        consolidate: bool,
    ) -> Option<usize> {
        let name = self.bounce_name(source);
        let track = match track {
            Some(t) => t.min(self.tracks.len() - 1),
            None => self.claim_bounce_track(&name)?,
        };
        let lane = match source {
            BounceSource::Clips { patterns, audio } if consolidate => patterns
                .iter()
                .filter_map(|&i| self.clips.get(i).map(|c| c.lane))
                .chain(audio.iter().filter_map(|&i| self.audio_clips.get(i).map(|c| c.lane)))
                .min()
                .unwrap_or(0),
            _ => self.clips.iter().map(|c| c.lane + 1).chain(self.audio_clips.iter().map(|c| c.lane + 1)).max().unwrap_or(0).min(63),
        };
        let map = self.tempo_map();
        let len = if whole_file {
            let ticks = map.tick_at_secs(map.secs_at(start as f64) + secs) - start as f64;
            (ticks.ceil() as u32).max(end.saturating_sub(start)).max(1)
        } else {
            end.saturating_sub(start).max(1)
        };
        match (source, action) {
            (_, SourceAction::Keep) => {}
            (BounceSource::Channel(c), SourceAction::Mute) => {
                if let Some(ch) = self.channels.get_mut(*c) {
                    ch.muted = true;
                }
            }
            (BounceSource::Channel(c), SourceAction::Replace) => self.remove_channel(*c),
            (BounceSource::Clips { patterns, audio }, action) => {
                let mut patterns = patterns.clone();
                let mut audio = audio.clone();
                patterns.sort_unstable_by(|a, b| b.cmp(a));
                patterns.dedup();
                audio.sort_unstable_by(|a, b| b.cmp(a));
                audio.dedup();
                patterns.retain(|&i| i < self.clips.len());
                audio.retain(|&i| i < self.audio_clips.len());
                for i in patterns {
                    match action {
                        SourceAction::Mute => self.clips[i].muted = true,
                        _ => {
                            self.clips.remove(i);
                        }
                    }
                }
                for i in audio {
                    match action {
                        SourceAction::Mute => self.audio_clips[i].muted = true,
                        _ => {
                            self.audio_clips.remove(i);
                        }
                    }
                }
            }
            (BounceSource::Range(a, b), SourceAction::Mute) => self.mute_time(*a, *b),
            (BounceSource::Range(a, b), SourceAction::Replace) => self.clear_time(*a, *b),
        }
        self.audio_clips.push(AudioClipDef {
            path,
            lane,
            track,
            start,
            len,
            offset: 0.0,
            gain: 1.0,
            fade_in: 0.0,
            fade_out: 0.0,
            ..Default::default()
        });
        Some(self.audio_clips.len() - 1)
    }

    /// A free mixer track for a bounce, named after it, straight to the master.
    fn claim_bounce_track(&mut self, name: &str) -> Option<usize> {
        let t = self.free_or_new_track()?;
        let mut def = TrackDef::new(tr_fmt!("{} (bounce)", name));
        def.color = crate::BUS_COLORS[t % crate::BUS_COLORS.len()];
        def.id = self.tracks[t].id;
        self.tracks[t] = def;
        Some(t)
    }

    /// Cuts the clips that cross `tick` in two (the audio ones without a fade at the cut).
    pub fn split_time(&mut self, tick: u32) {
        let mut clips = Vec::with_capacity(self.clips.len() + 4);
        for c in &self.clips {
            if c.start < tick && tick < c.start + c.len {
                clips.push(ClipDef { len: tick - c.start, ..*c });
                clips.push(ClipDef { start: tick, len: c.start + c.len - tick, offset: c.offset + (tick - c.start), ..*c });
            } else {
                clips.push(*c);
            }
        }
        self.clips = clips;
        let map = self.tempo_map();
        let mut audio = Vec::with_capacity(self.audio_clips.len() + 4);
        for c in &self.audio_clips {
            if c.start < tick && tick < c.start + c.len {
                let offset = c.offset_at(tick as f64, &map);
                audio.push(AudioClipDef { len: tick - c.start, fade_out: 0.0, ..c.clone() });
                audio.push(AudioClipDef { start: tick, len: c.start + c.len - tick, offset, fade_in: 0.0, ..c.clone() });
            } else {
                audio.push(c.clone());
            }
        }
        self.audio_clips = audio;
    }

    /// Mutes everything that plays in `[a, b)`: clips are cut at the edges
    /// and their inside parts muted.
    pub fn mute_time(&mut self, a: u32, b: u32) {
        if b <= a {
            return;
        }
        self.split_time(a);
        self.split_time(b);
        for c in self.clips.iter_mut().filter(|c| c.start >= a && c.start + c.len <= b) {
            c.muted = true;
        }
        for c in self.audio_clips.iter_mut().filter(|c| c.start >= a && c.start + c.len <= b) {
            c.muted = true;
        }
    }
}

/// The first freeze whose content an edit from `old` to `new` changes (what
/// its render would sound like): a frozen channel's devices, notes, clips and
/// automation; a frozen track's inserts and everything that reaches it; the
/// tempo for both. Volume, pan and mute after the freeze stay live and do
/// not count. Thawing (or freezing) is not an edit of frozen content.
pub fn frozen_content_changed(old: &Project, new: &Project) -> Option<FreezeTarget> {
    let same_channels = old.channels.len() == new.channels.len();
    let tempo = |p: &Project| {
        (p.bpm, p.swing, p.tempo_changes.clone(), p.signatures.clone(), p.automation_lane(crate::AutoTarget::Tempo, crate::AutoScope::Song).cloned())
    };
    let tempo_changed = tempo(old) != tempo(new);
    // What a channel plays, before its fader.
    let channel = |p: &Project, c: usize| {
        let ch = &p.channels[c];
        let i = &ch.instrument;
        let notes: Vec<(u32, Option<Vec<ponzik_engine::Note>>)> = p.patterns.iter().map(|pat| (pat.len, pat.notes.get(c).cloned())).collect();
        let playing = |pat: usize| p.patterns.get(pat).and_then(|x| x.notes.get(c)).is_some_and(|n| !n.is_empty());
        let clips: Vec<ClipDef> = p.clips.iter().filter(|cl| playing(cl.pattern)).map(|cl| ClipDef { lane: 0, ..*cl }).collect();
        let lanes: Vec<crate::AutomationDef> = p
            .automation
            .iter()
            .filter(|a| matches!(a.target, crate::AutoTarget::InstrumentParam { channel, .. } if channel == c))
            .cloned()
            .collect();
        (i.kind.clone(), i.params.clone(), i.sample.clone(), i.pads.clone(), ch.note_fx.clone(), ch.modulators.clone(), notes, clips, lanes)
    };
    // What reaches a track, through its inserts.
    let track = |p: &Project, t: usize| {
        let def = &p.tracks[t];
        let members: Vec<usize> = (0..p.tracks.len()).filter(|&m| m != t && p.tracks[m].output == t).collect();
        let feeds = |x: usize| x == t || members.contains(&x);
        let channels: Vec<_> = (0..p.channels.len())
            .filter(|&c| feeds(p.channels[c].track))
            .map(|c| {
                let ch = &p.channels[c];
                (channel(p, c), ch.gain, ch.pan, ch.muted, ch.track)
            })
            .collect();
        let clips: Vec<AudioClipDef> =
            p.audio_clips.iter().filter(|a| feeds(a.track)).map(|a| AudioClipDef { lane: 0, color: 0, ..a.clone() }).collect();
        // Take groups: what plays (the comp or an auditioned take), not where they show.
        let takes: Vec<crate::TakeGroup> =
            p.take_groups.iter().filter(|g| feeds(g.track)).map(|g| crate::TakeGroup { lane: 0, folded: false, ..g.clone() }).collect();
        let lanes: Vec<crate::AutomationDef> = p
            .automation
            .iter()
            .filter(|a| match a.target {
                crate::AutoTarget::EffectParam { track, .. } => track == t,
                crate::AutoTarget::ChannelGain(c)
                | crate::AutoTarget::ChannelPan(c)
                | crate::AutoTarget::InstrumentParam { channel: c, .. }
                | crate::AutoTarget::PitchBend(c)
                | crate::AutoTarget::Cc { channel: c, .. } => {
                    p.channels.get(c).is_some_and(|ch| feeds(ch.track))
                }
                crate::AutoTarget::TrackGain(m) | crate::AutoTarget::TrackPan(m) => members.contains(&m),
                crate::AutoTarget::Tempo => false,
            })
            .cloned()
            .collect();
        let inserts: Vec<_> = def
            .inserts
            .iter()
            .map(|fx| fx.as_ref().map(|f| (f.kind.clone(), f.params.clone(), f.bypass, f.sidechain, f.rack.clone())))
            .collect();
        let member_mix: Vec<_> = members.iter().map(|&m| (m, p.tracks[m].gain, p.tracks[m].pan, p.tracks[m].muted, p.tracks[m].inserts.clone())).collect();
        (inserts, def.modulators.clone(), channels, clips, takes, lanes, member_mix)
    };
    for c in (0..old.channels.len()).filter(|&c| same_channels && old.channels[c].frozen.is_some()) {
        if new.channels[c].frozen == old.channels[c].frozen && (tempo_changed || channel(old, c) != channel(new, c)) {
            return Some(FreezeTarget::Channel(c));
        }
    }
    for t in (1..old.tracks.len().min(new.tracks.len())).filter(|&t| old.tracks[t].frozen.is_some()) {
        if new.tracks[t].frozen == old.tracks[t].frozen && (tempo_changed || track(old, t) != track(new, t)) {
            return Some(FreezeTarget::Track(t));
        }
    }
    None
}

/// Offline render of what a freeze replaces, from the start of the song,
/// tail included: the channel's instrument after its note effects (before
/// its volume and pan), or everything that reaches the track through its
/// inserts (before its fader).
pub fn render_freeze(project: &Project, target: FreezeTarget, sample_rate: u32, loader: SampleLoader) -> Vec<f32> {
    let mut p = project.clone();
    let tap = match target {
        FreezeTarget::Channel(c) if c < p.channels.len() => {
            p.channels[c].frozen = None;
            p.channels[c].muted = false;
            let t = p.channels[c].track;
            if let Some(track) = p.tracks.get_mut(t) {
                track.frozen = None;
            }
            Tap::Channel(c)
        }
        FreezeTarget::Track(t) if t < p.tracks.len() => {
            p.tracks[t].frozen = None;
            Tap::Track(t)
        }
        _ => return Vec::new(),
    };
    render_taps(&p, sample_rate, loader, RenderMode::Song, &[tap]).pop().unwrap_or_default()
}

/// Renders a bounce: only the source sounds. A channel without its effects
/// is what it sends to its mixer track (the clip goes there); audio clips of
/// one track without effects are their sum (they are merged on that track);
/// otherwise the render is what the source adds to the mix, through the
/// mixer, with or without the inserts, for a new track.
pub fn render_bounce(
    project: &Project,
    source: &BounceSource,
    with_fx: bool,
    sample_rate: u32,
    loader: SampleLoader,
) -> BounceRender {
    let (start, end) = project.bounce_range(source);
    let mut p = project.clone();
    p.loop_range = None;
    // Solo renders hear the sources live: a frozen render would play whatever is muted.
    let thaw = |p: &mut Project| {
        p.channels.iter_mut().for_each(|c| c.frozen = None);
        p.tracks.iter_mut().for_each(|t| t.frozen = None);
    };
    let strip = |p: &mut Project, t: usize| p.tracks[t].inserts = Default::default();
    let (tap, track, mode) = match source {
        BounceSource::Channel(c) if !with_fx => {
            let c = (*c).min(p.channels.len().saturating_sub(1));
            if let Some(ch) = p.channels.get_mut(c) {
                ch.muted = false;
                let t = ch.track;
                p.tracks[t].frozen = None;
            }
            (Tap::ChannelOut(c), p.channels.get(c).map(|ch| ch.track), RenderMode::Song)
        }
        BounceSource::Channel(c) => {
            thaw(&mut p);
            for (i, ch) in p.channels.iter_mut().enumerate() {
                ch.muted = i != *c;
            }
            p.audio_clips.iter_mut().for_each(|a| a.muted = true);
            p.take_groups.iter_mut().for_each(|g| g.muted = true);
            (Tap::Track(MASTER), None, RenderMode::Song)
        }
        BounceSource::Clips { patterns, audio } => {
            thaw(&mut p);
            p.clips = patterns.iter().filter_map(|&i| project.clips.get(i)).map(|c| ClipDef { muted: false, ..*c }).collect();
            for (i, a) in p.audio_clips.iter_mut().enumerate() {
                a.muted = !audio.contains(&i);
            }
            p.take_groups.iter_mut().for_each(|g| g.muted = true);
            let tracks: Vec<usize> = audio.iter().filter_map(|&i| project.audio_clips.get(i)).map(|a| a.track).collect();
            let one_track = tracks.first().filter(|&&t| tracks.iter().all(|&x| x == t)).copied();
            match one_track.filter(|_| !with_fx && patterns.is_empty()) {
                Some(t) => {
                    strip(&mut p, t);
                    (Tap::Track(t), Some(t), RenderMode::Region(start, end))
                }
                None => {
                    if !with_fx {
                        (0..p.tracks.len()).for_each(|t| strip(&mut p, t));
                    }
                    (Tap::Track(MASTER), None, RenderMode::Region(start, end))
                }
            }
        }
        BounceSource::Range(..) => {
            if !with_fx {
                thaw(&mut p);
                (0..p.tracks.len()).for_each(|t| strip(&mut p, t));
            }
            (Tap::Track(MASTER), None, RenderMode::Region(start, end))
        }
    };
    let audio = render_taps(&p, sample_rate, loader, mode, &[tap]).pop().unwrap_or_default();
    BounceRender { audio, start, end, track }
}

/// The render as a sample, to play it before its file is read back.
pub fn render_sample(name: &str, audio: &[f32], sample_rate: u32) -> Sample {
    let (left, right) = audio.chunks_exact(2).map(|f| (f[0], f[1])).unzip();
    Sample { name: name.into(), sample_rate: sample_rate as f32, left, right, markers: Vec::new() }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::HashMap;

    use ponzik_engine::{Note, TICKS_PER_BAR, TICKS_PER_STEP};

    use super::*;
    use crate::render::{render_song, write_render};
    use crate::{EffectDef, InstrumentDef, NoteFxDef};

    const SR: u32 = 22_050;

    /// Files written by the tests, served back by path.
    #[derive(Default)]
    struct Files(RefCell<HashMap<String, Arc<Sample>>>);

    impl Files {
        fn add(&self, path: &str, audio: &[f32]) -> String {
            self.0.borrow_mut().insert(path.into(), Arc::new(render_sample(path, audio, SR)));
            path.into()
        }

        fn get(&self, path: &str) -> Option<Arc<Sample>> {
            self.0.borrow().get(path).cloned()
        }
    }

    fn peak(s: &[f32]) -> f32 {
        s.iter().fold(0.0f32, |m, v| m.max(v.abs()))
    }

    /// Largest difference, the shorter render padded with silence.
    fn worst(a: &[f32], b: &[f32]) -> f32 {
        worst_outside(a, b, &[], 0.0)
    }

    /// Like [`worst`], frames within `secs` of `edges` left out: at a clip's
    /// edges the engine always fades (2 ms at least, against clicks), and a
    /// bounced clip fades what its source had already faded.
    fn worst_outside(a: &[f32], b: &[f32], edges: &[usize], secs: f64) -> f32 {
        let fade = (secs * SR as f64) as usize;
        (0..a.len().max(b.len()))
            .filter(|i| edges.iter().all(|&e| (i / 2).abs_diff(e) > fade))
            .map(|i| (a.get(i).unwrap_or(&0.0) - b.get(i).unwrap_or(&0.0)).abs())
            .fold(0.0, f32::max)
    }

    /// Two channels with effects on their tracks (a latent limiter, an echo
    /// tail), an arpeggiated chord, played twice in the song.
    fn project() -> Project {
        let mut p = Project::empty();
        p.bpm = 130.0;
        let a = p.add_channel("Lead", InstrumentDef::new("pulse"), 60);
        let b = p.add_channel("Bass", InstrumentDef::new("pulse"), 48);
        p.channels[a].note_fx = vec![Some(NoteFxDef::new("arpeggiator"))];
        p.channels[a].gain = 0.6;
        p.channels[a].pan = -0.3;
        for k in [60, 64, 67] {
            p.add_note(0, a, Note::new(0, TICKS_PER_BAR / 2, k, 0.7));
        }
        for s in [0, 4, 8, 12] {
            p.add_note(0, b, Note::new(s * TICKS_PER_STEP, TICKS_PER_STEP * 2, 36, 0.8));
        }
        let ta = p.channels[a].track;
        let tb = p.channels[b].track;
        let mut echo = EffectDef::new("delay");
        echo.set(2, 0.5);
        p.tracks[ta].inserts[0] = Some(echo);
        p.tracks[ta].inserts[1] = Some(EffectDef::new("limiter"));
        p.tracks[tb].inserts[0] = Some(EffectDef::new("saturator"));
        p.tracks[ta].gain = 0.8;
        for bar in [0, 2] {
            p.clips.push(ClipDef { pattern: 0, lane: 0, start: bar * TICKS_PER_BAR, len: TICKS_PER_BAR, offset: 0, muted: false });
        }
        p.mode = ponzik_engine::PlayMode::Song;
        p
    }

    #[test]
    fn frozen_channel_sounds_like_the_live_one() {
        let p = project();
        let files = Files::default();
        let loader = |path: &str| files.get(path);
        let live = render_song(&p, SR, &loader, RenderMode::Song);
        assert!(peak(&live) > 0.05);

        let freeze = render_freeze(&p, FreezeTarget::Channel(0), SR, &loader);
        assert!(freeze.len() / 2 >= crate::render::ticks_to_frames(p.song_len(), p.bpm, SR), "the whole song");
        let mut frozen = p.clone();
        frozen.set_frozen(FreezeTarget::Channel(0), Some(files.add("lead.wav", &freeze)));
        assert!(frozen.channel_locked(0) && !frozen.channel_live(0, &loader));
        // The instrument and its note effects are unloaded from the engine.
        let (engine, _) = frozen.build_engine(SR as f32, &loader);
        let ch = &engine.channels()[0];
        assert_eq!(ch.instrument.kind(), "silence");
        assert!(ch.note_fx.iter().all(Option::is_none) && ch.frozen.is_some());

        let played = render_song(&frozen, SR, &loader, RenderMode::Song);
        let body = 2 * crate::render::ticks_to_frames(p.song_len(), p.bpm, SR);
        assert!(live[..body] == played[..body], "the song itself is bit-exact");
        let w = worst(&live, &played);
        assert!(w < 1e-4, "tails differ by {w}");
        // Its fader stays live.
        frozen.channels[0].gain = 0.0;
        p_without(&p, 0, &frozen, &loader);
        // A missing file: the live devices take over.
        let mut lost = frozen.clone();
        lost.set_frozen(FreezeTarget::Channel(0), Some("gone.wav".into()));
        assert!(lost.channel_live(0, &loader));
        lost.channels[0].gain = p.channels[0].gain;
        assert!(render_song(&lost, SR, &loader, RenderMode::Song) == live);
    }

    /// `frozen` (channel `c` silenced by its fader) equals `p` without that channel.
    fn p_without(p: &Project, c: usize, frozen: &Project, loader: SampleLoader) {
        let mut muted = p.clone();
        muted.channels[c].muted = true;
        let w = worst(&render_song(&muted, SR, loader, RenderMode::Song), &render_song(frozen, SR, loader, RenderMode::Song));
        assert!(w < 1e-4, "{w}");
    }

    #[test]
    fn frozen_track_sounds_like_the_live_one_through_its_latency() {
        let p = project();
        let files = Files::default();
        let loader = |path: &str| files.get(path);
        let live = render_song(&p, SR, &loader, RenderMode::Song);
        let track = p.channels[0].track;
        let freeze = render_freeze(&p, FreezeTarget::Track(track), SR, &loader);
        let mut frozen = p.clone();
        frozen.set_frozen(FreezeTarget::Track(track), Some(files.add("track.wav", &freeze)));
        assert!(frozen.channel_locked(0) && !frozen.channel_locked(1));
        let (mut engine, _) = frozen.build_engine(SR as f32, &loader);
        assert_eq!(engine.channels()[0].instrument.kind(), "silence", "its channels are unloaded too");
        assert!(engine.channels()[0].frozen.is_none(), "the track's render covers them");
        let t = &engine.mixer_mut().tracks[track];
        assert!(t.inserts.iter().all(Option::is_none) && t.frozen.is_some());

        let played = render_song(&frozen, SR, &loader, RenderMode::Song);
        let w = worst(&live, &played);
        assert!(w < 1e-4, "frozen track differs by {w}");
        // The fader after the freeze stays live.
        frozen.tracks[track].gain = 0.0;
        let quiet = render_song(&frozen, SR, &loader, RenderMode::Song);
        let mut silent = p.clone();
        silent.tracks[track].muted = true;
        assert!(worst(&quiet, &render_song(&silent, SR, &loader, RenderMode::Song)) < 1e-4);
        // A VCA or the master cannot be frozen.
        assert!(!p.can_freeze_track(MASTER));
        assert_eq!(frozen.set_frozen(FreezeTarget::Track(MASTER), Some("x".into())), None);
        assert!(!frozen.is_frozen(FreezeTarget::Track(MASTER)));
    }

    #[test]
    fn freeze_files_play_back_exactly() {
        let p = project();
        let none = |_: &str| None;
        let freeze = render_freeze(&p, FreezeTarget::Channel(1), SR, &none);
        let path = std::env::temp_dir().join(format!("ponzik-freeze-{}.wav", std::process::id()));
        // Hotter than full scale: kept as it is.
        let hot: Vec<f32> = freeze.iter().map(|s| s * 4.0).collect();
        write_render(&path, &hot, SR).unwrap();
        let back = ponzik_engine::wav::load_wav(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert!(peak(&hot) > 1.0);
        let again: Vec<f32> = back.left.iter().zip(&back.right).flat_map(|(&l, &r)| [l, r]).collect();
        assert!(again == hot);
    }

    #[test]
    fn edits_of_frozen_content_are_caught() {
        let mut p = project();
        let track = p.channels[0].track;
        p.set_frozen(FreezeTarget::Channel(0), Some("lead.wav".into()));
        let changed = |edit: &dyn Fn(&mut Project)| {
            let mut q = p.clone();
            edit(&mut q);
            frozen_content_changed(&p, &q)
        };
        let lead = Some(FreezeTarget::Channel(0));
        assert_eq!(changed(&|q| q.channels[0].instrument.set(2, 0.5)), lead);
        assert_eq!(changed(&|q| q.channels[0].note_fx.clear()), lead);
        assert_eq!(changed(&|q| q.add_note(0, 0, Note::new(96, 24, 72, 1.0))), lead);
        assert_eq!(changed(&|q| q.clips[1].start += TICKS_PER_BAR), lead);
        assert_eq!(changed(&|q| q.bpm = 90.0), lead);
        let param = crate::AutoTarget::InstrumentParam { channel: 0, id: 1 };
        assert_eq!(changed(&|q| { q.add_automation_point(param, crate::AutoScope::Song, 0, 0.5); }), lead);
        // Live after the freeze: fader, pan, mute, its track; and other channels' notes.
        assert_eq!(changed(&|q| q.channels[0].gain = 0.1), None);
        assert_eq!(changed(&|q| q.channels[0].muted = true), None);
        assert_eq!(changed(&|q| q.tracks[track].inserts[0] = None), None);
        assert_eq!(changed(&|q| q.add_note(0, 1, Note::new(96, 24, 40, 1.0))), None);
        assert_eq!(changed(&|q| q.clips[1].lane = 5), None, "a clip moved to another lane plays the same");
        // Thawing is not an edit.
        assert_eq!(changed(&|q| q.channels[0].frozen = None), None);

        // A frozen track: its inserts and everything that reaches it.
        let mut p = project();
        p.set_frozen(FreezeTarget::Track(track), Some("t.wav".into()));
        let changed = |edit: &dyn Fn(&mut Project)| {
            let mut q = p.clone();
            edit(&mut q);
            frozen_content_changed(&p, &q)
        };
        let frozen = Some(FreezeTarget::Track(track));
        assert_eq!(changed(&|q| q.tracks[track].inserts[0] = None), frozen);
        assert_eq!(changed(&|q| q.channels[0].gain = 0.1), frozen, "the channel's fader is before the track's freeze");
        assert_eq!(changed(&|q| q.channels[0].instrument.set(2, 0.5)), frozen);
        assert_eq!(changed(&|q| q.audio_clips.push(AudioClipDef { track, ..Default::default() })), frozen);
        assert_eq!(changed(&|q| q.tracks[track].gain = 0.2), None, "the track's own fader stays live");
        assert_eq!(changed(&|q| q.channels[1].gain = 0.1), None);
    }

    #[test]
    fn freeze_state_is_saved() {
        let mut p = project();
        p.set_frozen(FreezeTarget::Channel(1), Some("/songs/Bass freeze.wav".into()));
        p.set_frozen(FreezeTarget::Track(5), Some("/songs/Track 5 freeze.wav".into()));
        p.clips[1].muted = true;
        let back = crate::load(&crate::save(&p)).unwrap();
        assert_eq!(back.channels[1].frozen.as_deref(), Some("/songs/Bass freeze.wav"));
        assert_eq!(back.tracks[5].frozen.as_deref(), Some("/songs/Track 5 freeze.wav"));
        assert!(back.clips[1].muted && !back.clips[0].muted);
        assert_eq!(back.freeze_paths().count(), 2);
        assert!(!crate::save(&project()).contains("frozen"));
        assert!(!crate::save(&project()).contains("muted\": true"));
    }

    #[test]
    fn dry_channel_bounce_replaces_the_channel_in_place() {
        let p = project();
        let none = |_: &str| None;
        let live = render_song(&p, SR, &none, RenderMode::Song);
        let source = BounceSource::Channel(0);
        let r = render_bounce(&p, &source, false, SR, &none);
        assert_eq!((r.start, r.end, r.track), (0, p.song_len(), Some(p.channels[0].track)));
        let files = Files::default();
        let path = files.add("lead bounce.wav", &r.audio);
        let mut q = p.clone();
        let secs = r.audio.len() as f64 / 2.0 / SR as f64;
        let clip = q.place_bounce(&source, r.track, (r.start, r.end), secs, path, SourceAction::Mute, true, false).unwrap();
        let c = &q.audio_clips[clip];
        assert_eq!((c.track, c.start, c.lane), (p.channels[0].track, 0, 1), "on the channel's track, on a new lane");
        assert!(c.len >= p.song_len(), "the tail is in the clip");
        assert!(q.channels[0].muted);
        let loader = |path: &str| files.get(path);
        let played = render_song(&q, SR, &loader, RenderMode::Song);
        // Equal but for the clip's 2 ms fade-in at the very start, which the
        // limiter on the track still hears a little, and the echo brings back
        // (the delay repeats every 0.23 s or so).
        let echo = (0.2 * SR as f64) as usize;
        let w = worst_outside(&live[..2 * echo], &played[..2 * echo], &[0], 0.003);
        assert!(w < 5e-4, "{w}");
        let w = worst_outside(&live, &played, &[0], 0.003);
        assert!(w < 5e-3, "{w}");
        // Replace: the channel goes.
        let mut q = p.clone();
        q.place_bounce(&source, r.track, (r.start, r.end), secs, "x.wav".into(), SourceAction::Replace, true, false);
        assert_eq!(q.channels.len(), 1);
    }

    #[test]
    fn clip_bounce_with_effects_lands_on_a_new_track_at_the_clip() {
        let mut p = project();
        // No echo and no arpeggiator: the clips do not overlap, and nothing
        // carries on from one to the other (the arpeggiator's step, the echo).
        let ta = p.channels[0].track;
        p.tracks[ta].inserts[0] = None;
        p.channels[0].note_fx.clear();
        let none = |_: &str| None;
        let source = BounceSource::Clips { patterns: vec![1], audio: Vec::new() };
        let r = render_bounce(&p, &source, true, SR, &none);
        assert_eq!((r.start, r.end, r.track), (2 * TICKS_PER_BAR, 3 * TICKS_PER_BAR, None));
        let bar = crate::render::ticks_to_frames(TICKS_PER_BAR, p.bpm, SR);
        assert!(r.audio.len() / 2 >= bar, "the whole bar: {} / {bar}", r.audio.len() / 2);
        assert!(peak(&r.audio[..2 * bar]) > 0.05);
        let mut q = p.clone();
        let secs = r.audio.len() as f64 / 2.0 / SR as f64;
        let clip = q.place_bounce(&source, r.track, (r.start, r.end), secs, "b.wav".into(), SourceAction::Mute, true, false).unwrap();
        let c = q.audio_clips[clip].clone();
        assert_eq!((c.start, c.lane), (2 * TICKS_PER_BAR, 1));
        assert!(c.len > TICKS_PER_BAR);
        let t = &q.tracks[c.track];
        assert_eq!(t.name, "Lead (bounce)".replace("Lead", &q.patterns[0].name));
        assert!(t.inserts.iter().all(Option::is_none) && t.output == MASTER);
        assert!(!p.track_in_use(c.track), "a track that was free");
        assert!(q.clips[1].muted && !q.clips[0].muted);
        assert_eq!(q.sequence().clips.len(), 1, "a muted clip does not play");
        // The bounce is what the clip added to the mix: the song sounds the same.
        let files = Files::default();
        q.audio_clips[clip].path = files.add("b.wav", &r.audio);
        let loader = |path: &str| files.get(path);
        let live = render_song(&p, SR, &none, RenderMode::Song);
        let edge = crate::render::ticks_to_frames(2 * TICKS_PER_BAR, p.bpm, SR);
        let played = render_song(&q, SR, &loader, RenderMode::Song);
        let w = worst_outside(&live, &played, &[edge], 0.003);
        // What differs is the state of the effects: the limiter and the
        // saturator heard the first pass in the song, not in the bounce.
        assert!(w < 1e-3, "{w}");
    }

    /// Two audio clips on one track, consolidated into one.
    #[test]
    fn consolidate_merges_audio_clips_on_their_track() {
        let mut p = project();
        p.clips.clear();
        let tone: Vec<f32> = (0..SR as usize).flat_map(|i| [(i as f32 * 0.05).sin() * 0.4; 2]).collect();
        let files = Files::default();
        let a = files.add("a.wav", &tone);
        let track = 7;
        let mut gain = EffectDef::new("utility");
        gain.set(0, 6.0);
        p.tracks[track].inserts[0] = Some(gain);
        let clip = |start, lane| AudioClipDef { path: a.clone(), lane, track, start, len: TICKS_PER_BAR / 2, fade_in: 0.0, fade_out: 0.0, ..Default::default() };
        p.audio_clips.push(clip(TICKS_PER_BAR, 2));
        p.audio_clips.push(clip(2 * TICKS_PER_BAR, 3));
        // Another clip elsewhere stays out of it.
        p.audio_clips.push(AudioClipDef { track: 9, ..clip(TICKS_PER_BAR, 4) });
        let loader = |path: &str| files.get(path);
        let live = render_song(&p, SR, &loader, RenderMode::Song);

        let source = BounceSource::Clips { patterns: Vec::new(), audio: vec![0, 1] };
        let r = render_bounce(&p, &source, false, SR, &loader);
        assert_eq!((r.start, r.end, r.track), (TICKS_PER_BAR, 5 * TICKS_PER_BAR / 2, Some(track)));
        // The merged clip holds both clips and the gap between them, without the track's effect.
        let at = |tick: u32| crate::render::ticks_to_frames(tick - TICKS_PER_BAR, p.bpm, SR);
        let first = &r.audio[2 * at(TICKS_PER_BAR)..2 * at(3 * TICKS_PER_BAR / 2)];
        let fade = 2 * (0.003 * SR as f64) as usize;
        assert!(worst(&first[fade..first.len() - fade], &tone[fade..first.len() - fade]) < 1e-5);
        assert!(peak(&r.audio[2 * at(3 * TICKS_PER_BAR / 2) + 8..2 * at(2 * TICKS_PER_BAR) - 8]) < 1e-6, "the gap is silent");

        let path = files.add("merged.wav", &r.audio);
        let mut q = p.clone();
        let secs = r.audio.len() as f64 / 2.0 / SR as f64;
        let i = q.place_bounce(&source, r.track, (r.start, r.end), secs, path, SourceAction::Replace, false, true).unwrap();
        assert_eq!(q.audio_clips.len(), 2);
        let c = &q.audio_clips[i];
        assert_eq!((c.start, c.len, c.lane, c.track), (TICKS_PER_BAR, 3 * TICKS_PER_BAR / 2, 2, track));
        let edges = [at(TICKS_PER_BAR), at(5 * TICKS_PER_BAR / 2)].map(|f| f + crate::render::ticks_to_frames(TICKS_PER_BAR, p.bpm, SR));
        let played = render_song(&q, SR, &loader, RenderMode::Song);
        let w = worst_outside(&live, &played, &edges, 0.02);
        assert!(w < 1e-5, "{w}");
    }

    #[test]
    fn render_selection_goes_to_a_new_track_and_mutes_the_range() {
        let p = project();
        let none = |_: &str| None;
        let (a, b) = (TICKS_PER_BAR / 2, 5 * TICKS_PER_BAR / 2);
        let source = BounceSource::Range(a, b);
        let r = render_bounce(&p, &source, true, SR, &none);
        assert_eq!((r.start, r.end, r.track), (a, b, None));
        let mut q = p.clone();
        let secs = r.audio.len() as f64 / 2.0 / SR as f64;
        let i = q.place_bounce(&source, None, (a, b), secs, "sel.wav".into(), SourceAction::Mute, false, false).unwrap();
        let c = &q.audio_clips[i];
        assert_eq!((c.start, c.len), (a, b - a));
        assert_ne!(c.track, p.channels[0].track);
        assert_eq!(q.tracks[c.track].name, "Selection (bounce)");
        // Clips cut at the edges: what is inside is muted, the rest plays.
        let spans: Vec<(u32, u32, bool)> = q.clips.iter().map(|c| (c.start, c.len, c.muted)).collect();
        let half = TICKS_PER_BAR / 2;
        assert_eq!(spans, [(0, half, false), (half, half, true), (2 * TICKS_PER_BAR, half, true), (5 * half, half, false)]);
        assert_eq!(q.clips[3].offset, half);
        // Replace: the range is emptied.
        let mut q = p.clone();
        q.place_bounce(&source, None, (a, b), secs, "sel.wav".into(), SourceAction::Replace, false, false);
        assert_eq!(q.clips.len(), 2);
    }
}
