//! MIDI capture in the interface: Ctrl+Shift+C turns the last phrase played
//! (controller or typing keyboard), recording or not, into a new pattern.

use ponzik_i18n::{tr, tr_fmt, tr_fmt_n};
use ponzik_project::capture::capture;

use super::app::{Action, App};

impl App {
    pub fn capture_midi(&mut self) {
        let now = self.ctl.now();
        let phrase = self.note_log.last_phrase(now);
        let bar = self.sig_here().bar_len();
        let Some(cap) = capture(&phrase, self.tempo_here(), bar) else {
            self.toast(tr("Nothing to capture: play something on a controller or the typing keyboard"));
            return;
        };
        let p = self.project.add_pattern();
        let n = self.project.patterns.iter().filter(|q| q.name.starts_with(tr("Capture"))).count() + 1;
        self.project.patterns[p].name = format!("{} {n}", tr("Capture"));
        self.project.patterns[p].len = cap.len;
        let channels = self.project.channels.len();
        let mut count = 0;
        for (ch, note) in cap.notes {
            if ch < channels {
                self.project.add_note(p, ch, note);
                count += 1;
            }
        }
        if cap.detected {
            self.set_tempo_here(cap.bpm);
        }
        self.project.current_pattern = p;
        self.playlist.brush = p;
        self.piano.selected.clear();
        if let ponzik_engine::PlayMode::Pattern(_) = self.project.mode {
            self.project.mode = ponzik_engine::PlayMode::Pattern(p);
        }
        self.break_undo_merge();
        self.commit(tr("Capture MIDI"));
        let notes = tr_fmt_n!("{} note", "{} notes", count, count);
        let text = if cap.detected {
            tr_fmt!("Captured {} at {} BPM (detected)", notes, ponzik_i18n::num(cap.bpm as f64, 1))
        } else {
            tr_fmt!("Captured {} into \"{}\"", notes, self.project.patterns[p].name)
        };
        self.toast_action(&text, tr("Undo"), Action::Undo);
    }

    /// `PONZIK_CAPTURE=1`: a phrase played loosely on the typing keyboard,
    /// transport stopped, then captured.
    pub fn screenshot_capture(&mut self) {
        self.stop();
        let ch = self.selected_channel().unwrap_or(0);
        let now = self.ctl.now() - 20.0;
        let beat = 60.0 / 108.0;
        let keys = [57u8, 60, 64, 62, 60, 57, 55, 57, 60, 64, 67, 64];
        let steps = [0, 2, 3, 4, 6, 8, 10, 11, 12, 14, 15, 16];
        for (i, (&k, &s)) in keys.iter().zip(&steps).enumerate() {
            let t = now + s as f64 * beat / 2.0 + [0.0, 0.012, -0.008, 0.005][i % 4];
            self.note_log.note_on(ch, k, 0.6 + 0.3 * ((i % 3) as f32 / 2.0), t, None);
            self.note_log.note_off(ch, k, t + beat * 0.4);
        }
        self.capture_midi();
        self.focus = super::app::Focus::Channel(ch);
        self.set_view(super::app::View::Piano);
        self.toggle_play();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_makes_a_pattern_and_undo_removes_it() {
        let project = ponzik_project::demo_project();
        let (_engine, handle) = project.build_engine(48_000.0, &|_| None);
        let mut app = App::new(handle, project, 48_000);
        let patterns = app.project.patterns.len();
        app.capture_midi();
        assert_eq!(app.project.patterns.len(), patterns);
        let now = app.ctl.now();
        for i in 0..8 {
            let t = now - 5.0 + i as f64 * 0.25;
            app.note_log.note_on(1, 60 + i as u8, 0.8, t, None);
            app.note_log.note_off(1, 60 + i as u8, t + 0.1);
        }
        app.capture_midi();
        assert_eq!(app.project.patterns.len(), patterns + 1);
        let p = app.project.current_pattern;
        assert_eq!(app.project.patterns[p].notes[1].len(), 8);
        assert_eq!(app.project.patterns[p].notes[1][0].start, 0);
        app.undo();
        assert_eq!(app.project.patterns.len(), patterns);
    }
}
