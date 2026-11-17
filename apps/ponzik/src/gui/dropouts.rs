//! Audio dropouts: the driver counts its underruns (xruns), the interface
//! shows them in the DSP gauge tooltip, and when they repeat it raises the
//! audio buffer one step (10 → 15 → 20 ms, kept in the settings) or, past
//! that, suggests a larger buffer once.

use ponzik_i18n::{tr, tr_fmt};

use super::app::App;

/// Audio buffer choices offered in the settings, in milliseconds.
pub const BUFFER_STEPS: [u32; 6] = [5, 10, 15, 20, 30, 40];
/// The automatic raise stops here; beyond, the user decides.
const AUTO_MAX_MS: u32 = 20;
/// Dropouts this close together count as repeated…
const WINDOW: f32 = 10.0;
/// …from this many on.
const REPEATED: usize = 3;
/// Dropouts right after (re)opening the output are ignored (graph start-up).
const GRACE: f32 = 2.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XrunAction {
    Nothing,
    /// Suggest a larger buffer (once).
    Warn,
    /// Reopen the output with this buffer, in milliseconds.
    Raise(u32),
}

/// Watches the dropout counter of the output.
#[derive(Debug, Default)]
pub struct XrunWatch {
    seen: u32,
    recent: Vec<f32>,
    grace_until: f32,
    warned: bool,
}

/// Next automatic step above `ms`, if any.
pub fn next_auto_step(ms: u32) -> Option<u32> {
    BUFFER_STEPS.iter().copied().find(|&s| s > ms).filter(|&s| s <= AUTO_MAX_MS)
}

impl XrunWatch {
    /// The output was (re)opened at `now` (seconds) with `count` dropouts so far.
    pub fn reopened(&mut self, now: f32, count: u32) {
        self.seen = count;
        self.recent.clear();
        self.grace_until = now + GRACE;
    }

    /// Dropouts counted by the driver so far: `count`; current buffer `ms`;
    /// `auto`: the buffer may be raised automatically.
    pub fn update(&mut self, now: f32, count: u32, ms: u32, auto: bool) -> XrunAction {
        let new = count.saturating_sub(self.seen);
        self.seen = count;
        if new == 0 || now < self.grace_until {
            return XrunAction::Nothing;
        }
        self.recent.extend(std::iter::repeat_n(now, new.min(16) as usize));
        self.recent.retain(|&t| now - t <= WINDOW);
        if self.recent.len() < REPEATED {
            return XrunAction::Nothing;
        }
        self.recent.clear();
        if auto && let Some(next) = next_auto_step(ms) {
            return XrunAction::Raise(next);
        }
        if self.warned {
            return XrunAction::Nothing;
        }
        self.warned = true;
        XrunAction::Warn
    }
}

impl App {
    /// Dropouts counted by the output since it was opened.
    pub fn xruns(&self) -> u32 {
        self.rec.output.as_ref().map_or(0, |o| o.xruns())
    }

    /// Reads the dropout counter; true when the interface must redraw (toast).
    pub fn xrun_tick(&mut self, now: f32) -> bool {
        let count = self.xruns();
        // Recording in progress: no reopening under the take, just a warning.
        let recording = self.record && self.playing;
        let auto = !self.prefs.buffer_manual && !recording && std::env::var_os("PONZIK_LATENCY_MS").is_none();
        match self.xrun_watch.update(now, count, ponzik_audio::target_latency_ms(), auto) {
            XrunAction::Nothing => false,
            XrunAction::Warn => {
                self.toast(tr("Audio dropouts: a larger buffer in Settings should stop the crackles"));
                true
            }
            XrunAction::Raise(ms) => {
                if self.reopen_output(ms, now) {
                    self.prefs.buffer_ms = ms;
                    self.save_settings();
                    self.toast(&tr_fmt!("Audio dropouts: buffer raised to {} ms", ms));
                }
                true
            }
        }
    }

    /// Buffer chosen in the settings (`None`: automatic, starting at 10 ms).
    pub fn set_audio_buffer(&mut self, ms: Option<u32>, now: f32) {
        let auto = ms.is_none();
        let ms = ms.unwrap_or(ponzik_audio::DEFAULT_LATENCY_MS);
        let changed = ms != ponzik_audio::target_latency_ms();
        (self.prefs.buffer_ms, self.prefs.buffer_manual) = (ms, !auto);
        self.save_settings();
        if changed && self.reopen_output(ms, now) {
            self.toast(&tr_fmt!("Audio buffer: {} ms", ms));
        }
    }

    /// Reopens the output with a buffer of `ms` milliseconds, same engine.
    /// The input, if any, is detached; the next frame reattaches it.
    fn reopen_output(&mut self, ms: u32, now: f32) -> bool {
        let Some(output) = self.rec.output.take() else { return false };
        self.rec.input = None;
        super::set_pipewire_latency(ms);
        match output.reopen(ms) {
            Ok(stream) => {
                self.xrun_watch.reopened(now, stream.xruns());
                self.rec.output = Some(stream);
                true
            }
            Err(e) => {
                self.toast(&tr_fmt!("Could not reopen the audio output: {}", e.to_string()));
                false
            }
        }
    }

    /// Second part of the DSP gauge tooltip: buffer, dropouts, worker threads.
    pub fn audio_status(&self) -> String {
        let ms = ponzik_audio::target_latency_ms();
        let buffer = if !self.prefs.buffer_manual {
            tr_fmt!("Audio buffer: {} ms (automatic)", ms)
        } else {
            tr_fmt!("Audio buffer: {} ms", ms)
        };
        let drops = match self.xruns() {
            0 => tr("No dropouts").to_string(),
            n => tr_fmt!("Dropouts: {}", n),
        };
        let threads = match (self.engine.worker_threads(), self.engine.parallel_active()) {
            (0, _) => tr("Single-threaded engine").to_string(),
            (n, true) => tr_fmt!("{} worker threads sharing the work", n),
            (_, false) => tr("Worker threads asleep: one core is enough").to_string(),
        };
        format!("{buffer} · {drops}\n{threads}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_dropouts_raise_the_buffer_step_by_step_then_warn_once() {
        let mut w = XrunWatch::default();
        w.reopened(0.0, 0);
        // During the grace period after opening: ignored.
        assert_eq!(w.update(1.0, 5, 10, true), XrunAction::Nothing);
        // A lone dropout, then another one much later: nothing.
        assert_eq!(w.update(3.0, 6, 10, true), XrunAction::Nothing);
        assert_eq!(w.update(20.0, 7, 10, true), XrunAction::Nothing);
        // Three within ten seconds: one step up.
        assert_eq!(w.update(22.0, 8, 10, true), XrunAction::Nothing);
        assert_eq!(w.update(23.0, 9, 10, true), XrunAction::Raise(15));
        w.reopened(23.0, 9);
        assert_eq!(w.update(30.0, 12, 15, true), XrunAction::Raise(20));
        w.reopened(30.0, 12);
        // At 20 ms the automatic raise stops: a single suggestion.
        assert_eq!(w.update(40.0, 20, 20, true), XrunAction::Warn);
        assert_eq!(w.update(50.0, 30, 20, true), XrunAction::Nothing);
        // A buffer chosen by hand is never raised: the warning instead.
        let mut w = XrunWatch::default();
        assert_eq!(w.update(10.0, 3, 5, false), XrunAction::Warn);
        // A counter that goes back (new output) counts nothing.
        assert_eq!(w.update(11.0, 0, 5, false), XrunAction::Nothing);
    }

    #[test]
    fn automatic_steps() {
        assert_eq!(next_auto_step(5), Some(10));
        assert_eq!(next_auto_step(10), Some(15));
        assert_eq!(next_auto_step(15), Some(20));
        assert_eq!(next_auto_step(20), None);
        assert_eq!(next_auto_step(12), Some(15));
        assert_eq!(next_auto_step(40), None);
    }
}
