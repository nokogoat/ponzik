//! Resampling (Ableton's "Resampling" input, FL Studio's master recording):
//! an armed track records the master's output, or another track's output
//! (post-fader), in real time while the song plays. The take comes back
//! aligned on the timeline by the latency of what it recorded, like any take.
//!
//! No loop: the engine cuts the armed track's own output while it records a
//! resampled output (it is routed nowhere), and a track can't record itself.
//! The offline counterpart, "Render selection to audio", lives in `freeze.rs`.

use ponzik_engine::InputSource;
use ponzik_engine::mixer::MASTER;
use ponzik_i18n::{tr, tr_fmt};
use ponzik_project::TrackKind;
use ponzik_ui::{Canvas, Rect};

use super::Input;
use super::app::{Action, App, MenuItem, Popup};
use super::widgets::{Icon, draw_icon, wid};

impl App {
    /// Source given to the tracks armed now: the resampled output, or the input.
    pub(super) fn arm_source(&self) -> InputSource {
        self.rec.resample.map_or(self.rec.settings.source, InputSource::Resample)
    }

    /// Name of a resampling source.
    pub(super) fn resample_name(&self, track: usize) -> String {
        if track == MASTER {
            tr("Master (resampling)").to_string()
        } else {
            let name = self.project.tracks.get(track).map_or_else(|| ponzik_project::default_track_name(track), |t| t.name.clone());
            tr_fmt!("{} output", name)
        }
    }

    /// The record panel's source menu: the input, the master, every track that plays.
    pub(super) fn record_source_items(&self) -> (String, Vec<MenuItem>) {
        let now = self.rec.resample;
        let mut v = vec![
            MenuItem::new(Icon::Record, tr("Audio input"), Action::SetResample(None)).on(now.is_none()).tip(tr("Record the microphone or line input")),
            MenuItem::new(Icon::Mixer, &self.resample_name(MASTER), Action::SetResample(Some(MASTER)))
                .on(now == Some(MASTER))
                .tip(tr("Record what you hear, in time with the song\nThe armed track is routed nowhere while it records"))
                .sep(),
        ];
        for (t, track) in self.project.tracks.iter().enumerate().skip(1) {
            if track.kind == TrackKind::Vca || !self.project.track_in_use(t) {
                continue;
            }
            let armed = self.rec.is_armed(t);
            let item = MenuItem::new(Icon::Route, &self.resample_name(t), if armed { Action::None } else { Action::SetResample(Some(t)) })
                .on(now == Some(t))
                .dim(armed)
                .detail(if armed { tr("armed").to_string() } else { t.to_string() });
            v.push(if armed {
                item.tip(tr("Armed: a track can't record its own output"))
            } else {
                item.tip(tr("Record this track after its fader, in time with the song"))
            });
        }
        (tr("Record from").into(), v)
    }

    /// Records the armed tracks from `source` (`None`: the audio input). A
    /// track armed on the output it would record is disarmed.
    pub(super) fn set_resample(&mut self, source: Option<usize>) {
        if let Some(t) = source.filter(|&t| t != MASTER && self.rec.is_armed(t)) {
            self.toggle_arm(t);
        }
        let old = self.rec.armed.clone();
        self.rec.resample = source;
        let new = self.arm_source();
        for a in &mut self.rec.armed {
            a.1 = new;
        }
        // Tracks that were cut for a resampling take play again.
        for (track, s) in old {
            if s.resampled().is_some() && source.is_none() {
                self.send(ponzik_engine::Command::SetMonitor { track, source: None });
            }
        }
        self.update_monitors();
        if let Some(t) = source {
            self.toast(&tr_fmt!("Armed tracks now record {}", self.resample_name(t)));
        }
    }

    /// "Resample the master": a new track, armed on the master's output.
    pub fn resample_master(&mut self) {
        let Some(track) = self.add_mixer_track() else { return };
        self.project.tracks[track].name = tr("Resampling").to_string();
        self.commit(tr("Rename track"));
        if self.rec.resample != Some(MASTER) {
            self.set_resample(Some(MASTER));
        }
        if !self.rec.is_armed(track) {
            self.toggle_arm(track);
        }
    }

    /// The panel's row: what the armed tracks record, a menu on click.
    pub(super) fn record_source_row(&mut self, c: &mut Canvas, input: &Input, r: Rect) {
        let t = self.theme;
        let id = wid("rec-resample", 0, 0);
        let hov = self.hovered(input, id, r);
        let hv = self.hover_anim(id, hov);
        let on = self.rec.resample.is_some();
        c.fill_rrect(r, 10.0, t.bg_deep.mix(t.raised, hv * 0.6).mix(t.accent_deep, if on { 0.35 } else { 0.0 }));
        c.stroke_rrect(r, 10.0, 1.0, if on { t.accent.alpha(0.5) } else { t.hairline });
        let (_, cy) = r.center();
        draw_icon(c, if on { Icon::Mixer } else { Icon::Record }, r.x + 16.0, cy, 14.0, if on { t.accent } else { t.muted });
        let label = match self.rec.resample {
            Some(s) => self.resample_name(s),
            None => tr("Audio input").to_string(),
        };
        self.text(c, self.f.sans, 11.0, r.x + 32.0, cy, tr("Record from"), t.muted);
        let lw = self.fonts.measure(self.f.sans, 11.0, tr("Record from"));
        self.text_fit(c, self.f.sans_med, 12.0, r.x + 42.0 + lw, cy, &label, r.w - lw - 70.0, t.text);
        draw_icon(c, Icon::ChevronDown, r.right() - 16.0, cy, 12.0, t.text2);
        if hov {
            self.set_tip(tr("Record the audio input, or resample the master or a track's output"), "");
            if input.pressed {
                self.open_popup(Popup::RecordSource, Rect::new(r.x, r.bottom(), r.w, 0.0));
            }
        }
    }
}
