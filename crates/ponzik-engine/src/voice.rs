//! Voice to notes in the engine: the audio input, tracked on the audio
//! thread ([`ponzik_dsp::voice`]), plays the chosen channel's instrument
//! live, through the same path as a controller's notes (its note effects
//! included: hum one note, get a chord). Notes start on their sample within
//! the block, right after the input that triggered them; nothing allocates.
//!
//! What the tracker hears and plays goes back to the interface
//! ([`VoiceReport`]): the pitch trail, and the notes with their song position
//! for recording into patterns and MIDI capture.

pub use ponzik_dsp::voice::{VoiceConfig, VoiceEvent, VoiceFrame, VoiceMode};
use ponzik_dsp::voice::VoiceTracker;

use crate::input::InputSource;
use crate::note_fx::NoteEvent;
use crate::rack::Channel;
use crate::ring::{self, Consumer, Producer};

/// Note events of one block, at most (the tracker emits a few per hop).
const MAX_EVENTS: usize = 256;
/// Reports waiting for the interface: about ten seconds of pitch frames.
const REPORT_QUEUE: usize = 4_096;

/// Voice to notes settings, sent with [`crate::Command::SetVoice`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VoiceSettings {
    pub enabled: bool,
    /// Channel whose instrument plays the notes.
    pub channel: usize,
    /// Input channels listened to (stereo: their mix).
    pub source: InputSource,
    pub config: VoiceConfig,
}

impl Default for VoiceSettings {
    fn default() -> Self {
        Self { enabled: false, channel: 0, source: InputSource::Stereo, config: VoiceConfig::default() }
    }
}

/// What the engine tells the interface.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum VoiceReport {
    /// One analysis (every 2.5 ms).
    Frame(VoiceFrame),
    /// A note started on `channel`. `tick`: song position of its sample (NaN
    /// when the transport is stopped); `onset`: samples since the voice's
    /// attack (its reaction time).
    NoteOn { channel: usize, key: u8, velocity: f32, tick: f64, onset: u32 },
    NoteOff { channel: usize, key: u8, tick: f64 },
}

/// Interface side: the reports of the engine.
pub struct VoiceReports {
    rx: Consumer<VoiceReport>,
}

impl VoiceReports {
    pub fn drain(&mut self, mut f: impl FnMut(VoiceReport)) {
        while let Some(r) = self.rx.pop() {
            f(r);
        }
    }
}

pub(crate) struct VoiceInput {
    settings: VoiceSettings,
    tracker: VoiceTracker,
    mono: Vec<f32>,
    /// Events of the block: sample, event, song position (filled by the engine).
    pub(crate) events: Vec<(usize, VoiceEvent, f64)>,
    reports: Producer<VoiceReport>,
    /// Channel and keys sounding (two while legato notes overlap).
    playing: [Option<(usize, u8)>; 2],
    /// The channel's pitch bend was moved (instruments without note expression).
    bent: Option<usize>,
}

impl VoiceInput {
    pub(crate) fn new(sample_rate: f32, max_block: usize) -> (Self, VoiceReports) {
        let (tx, rx) = ring::channel(REPORT_QUEUE);
        let settings = VoiceSettings::default();
        let input = Self {
            settings,
            tracker: VoiceTracker::new(sample_rate, settings.config),
            mono: vec![0.0; max_block],
            events: Vec::with_capacity(MAX_EVENTS),
            reports: tx,
            playing: [None; 2],
            bent: None,
        };
        (input, VoiceReports { rx })
    }

    /// New settings: a note playing ends first when the voice is switched
    /// off or moves to another channel.
    pub(crate) fn set(&mut self, settings: VoiceSettings, channels: &mut [Channel], now: u64) {
        let old = self.settings;
        if old.enabled && (!settings.enabled || settings.channel != old.channel) {
            self.silence(channels, now);
        }
        if settings.config != old.config {
            self.tracker.set_config(settings.config);
        }
        self.settings = settings;
    }

    /// Ends the notes playing and forgets the voice (channels changed, input gone).
    pub(crate) fn silence(&mut self, channels: &mut [Channel], now: u64) {
        let mut ended = [None; 2];
        let mut k = 0;
        self.tracker.release(&mut |_, ev| {
            if let VoiceEvent::NoteOff { key } = ev {
                ended[k.min(1)] = Some(key);
                k += 1;
            }
        });
        for slot in 0..2 {
            if let Some((channel, key)) = self.playing[slot].take() {
                if let Some(ch) = channels.get_mut(channel) {
                    ch.note_in(now, NoteEvent::off(key));
                    ch.flush(now);
                }
                let _ = self.reports.push(VoiceReport::NoteOff { channel, key, tick: f64::NAN });
            }
        }
        self.unbend(channels);
        self.tracker.reset();
        self.events.clear();
    }

    /// Tracks a block of input; its note events land in `events`. Without
    /// input (closed), the note playing ends.
    pub(crate) fn analyze(&mut self, input: Option<(&[f32], &[f32])>) {
        self.events.clear();
        if !self.settings.enabled {
            return;
        }
        let Self { tracker, mono, events, reports, settings, .. } = self;
        let mut emit = |at: usize, ev: VoiceEvent| match ev {
            VoiceEvent::Frame(f) => {
                let _ = reports.push(VoiceReport::Frame(f));
            }
            ev => {
                if events.len() < events.capacity() {
                    events.push((at, ev, f64::NAN));
                }
            }
        };
        let Some((l, r)) = input else {
            tracker.release(&mut emit);
            return;
        };
        let n = l.len().min(mono.len());
        let mono = &mut mono[..n];
        match settings.source {
            InputSource::Stereo | InputSource::Resample(_) => {
                for (m, (a, b)) in mono.iter_mut().zip(l.iter().zip(r)) {
                    *m = 0.5 * (a + b);
                }
            }
            InputSource::Left => mono.copy_from_slice(&l[..n]),
            InputSource::Right => mono.copy_from_slice(&r[..n]),
        }
        tracker.process(mono, &mut emit);
    }

    /// Plays one event of the block on the channel, at sample `now`.
    pub(crate) fn apply(&mut self, ev: VoiceEvent, tick: f64, channels: &mut [Channel], now: u64) {
        let channel = self.settings.channel;
        match ev {
            VoiceEvent::NoteOn { key, velocity, onset } => {
                if let Some(ch) = channels.get_mut(channel) {
                    ch.note_in(now, NoteEvent::on(key, velocity));
                }
                let slot = usize::from(self.playing[0].is_some());
                if let Some((c, k)) = self.playing[slot].replace((channel, key))
                    && let Some(ch) = channels.get_mut(c)
                {
                    ch.note_in(now, NoteEvent::off(k));
                }
                if self.bent.is_some() {
                    self.unbend(channels);
                }
                let _ = self.reports.push(VoiceReport::NoteOn { channel, key, velocity, tick, onset });
            }
            VoiceEvent::NoteOff { key } => {
                let Some(slot) = self.playing.iter().position(|p| p.is_some_and(|(_, k)| k == key)) else { return };
                let Some((c, k)) = self.playing[slot].take() else { return };
                if let Some(ch) = channels.get_mut(c) {
                    ch.note_in(now, NoteEvent::off(k));
                }
                if self.playing.iter().all(Option::is_none) {
                    self.unbend(channels);
                }
                let _ = self.reports.push(VoiceReport::NoteOff { channel: c, key: k, tick });
            }
            VoiceEvent::Bend { key, semitones } => {
                if let Some(ch) = channels.get_mut(channel)
                    && self.playing.contains(&Some((channel, key)))
                {
                    let secs = self.tracker.hop_samples() as f32 / self.tracker.sample_rate();
                    if !ch.voice_bend(key, semitones, secs) {
                        self.bent = Some(channel);
                    }
                }
            }
            VoiceEvent::Frame(_) => {}
        }
    }

    /// Puts back the channel's pitch bend moved by the voice.
    fn unbend(&mut self, channels: &mut [Channel]) {
        if let Some(c) = self.bent.take()
            && let Some(ch) = channels.get_mut(c)
        {
            ch.instrument.controller(crate::Controller::PitchBend(0.0));
        }
    }
}
