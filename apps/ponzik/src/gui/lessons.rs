//! Learn: short, optional, hands-on lessons. Each step is one sentence in a
//! bubble anchored on the real widget it is about, with a soft pulse around
//! the widget; the step ends by itself when the user does the action (steps
//! switched on, a note drawn, a clip placed…) or with "Next". Nothing here is
//! ever forced: the panel opens from the menu, the palette or the top bar.
//!
//! Lessons are data: add one to `LESSONS` (steps = spot, sentence, goal), make
//! sure the views report its spots with `App::spot`, translate the strings.

use ponzik_i18n::{tr, tr_fmt, tr_noop};
use ponzik_project::Project;
use ponzik_project::json::Json;
use ponzik_project::scale::ScaleKind;
use ponzik_ui::{Canvas, Path, Rect};

use super::Input;
use super::app::{App, Focus, Popup, View};
use super::theme::*;
use super::widgets::{Icon, draw_icon, wid};

/// The widgets a step can point at. Views report where they drew them this
/// frame with [`App::spot`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Spot {
    TabRack,
    TabPiano,
    TabPlaylist,
    TabMixer,
    Play,
    Tempo,
    Mode,
    Scale,
    Record,
    Mic,
    Menu,
    Browser,
    BrowserTabs,
    BrowserList,
    RackSteps,
    RackSwing,
    PianoChannel,
    PianoGrid,
    PianoSnap,
    PianoChord,
    PlaylistPatterns,
    PlaylistGrid,
    Arrange,
    MixerStrip,
    MixerInserts,
    MixerArm,
    MasterStrip,
    MasterInserts,
}

impl Spot {
    #[cfg(test)]
    pub const ALL: [Spot; 28] = [
        Spot::TabRack,
        Spot::TabPiano,
        Spot::TabPlaylist,
        Spot::TabMixer,
        Spot::Play,
        Spot::Tempo,
        Spot::Mode,
        Spot::Scale,
        Spot::Record,
        Spot::Mic,
        Spot::Menu,
        Spot::Browser,
        Spot::BrowserTabs,
        Spot::BrowserList,
        Spot::RackSteps,
        Spot::RackSwing,
        Spot::PianoChannel,
        Spot::PianoGrid,
        Spot::PianoSnap,
        Spot::PianoChord,
        Spot::PlaylistPatterns,
        Spot::PlaylistGrid,
        Spot::Arrange,
        Spot::MixerStrip,
        Spot::MixerInserts,
        Spot::MixerArm,
        Spot::MasterStrip,
        Spot::MasterInserts,
    ];

    /// The view that shows the widget (none: the top bar or the browser).
    pub fn view(self) -> Option<View> {
        match self {
            Spot::RackSteps | Spot::RackSwing => Some(View::Rack),
            // In the piano roll's toolbar (and the Channel Rack's), or the top bar if kept there.
            Spot::Scale => Some(View::Piano),
            Spot::PianoChannel | Spot::PianoGrid | Spot::PianoSnap | Spot::PianoChord => Some(View::Piano),
            Spot::PlaylistPatterns | Spot::PlaylistGrid | Spot::Arrange => Some(View::Playlist),
            Spot::MixerStrip | Spot::MixerInserts | Spot::MixerArm | Spot::MasterStrip | Spot::MasterInserts => {
                Some(View::Mixer)
            }
            _ => None,
        }
    }

    fn in_browser(self) -> bool {
        matches!(self, Spot::BrowserTabs | Spot::BrowserList)
    }
}

/// What ends a step by itself. Counts are relative to the project when the
/// step started, so a lesson works on any project.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Goal {
    /// Only the "Next" button.
    Next,
    View(View),
    Playing,
    SongMode,
    /// Song mode and playing.
    SongPlaying,
    /// At least n more notes or steps.
    Notes(usize),
    /// A note made longer.
    Longer,
    /// A note or step erased.
    Erase,
    /// At least n more clips in the playlist.
    Clips(usize),
    /// A new channel or audio clip (a sample dropped).
    Sound,
    /// A recorded take (an audio clip more).
    Take,
    /// An effect inserted on a track.
    Effect,
    /// A mixer fader moved.
    Gain,
    /// A mixer pan moved.
    Pan,
    Tempo,
    Swing,
    /// The key or the scale changed.
    Scale,
    ScaleSnap,
    ChordStamp(bool),
    /// An arrangement section added.
    Sections,
    Browser,
    SamplesTab,
    RecordPanel,
    Armed,
    /// Recording armed and playing.
    Recording,
    MenuOpen,
    Exported,
}

pub struct Step {
    pub spot: Spot,
    /// One short sentence (marked with `tr_noop`, translated when drawn).
    pub text: &'static str,
    pub goal: Goal,
}

/// The two series of the Learn panel.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Series {
    /// How to use Ponzik.
    Basics,
    /// First productions.
    Production,
}

impl Series {
    pub const ALL: [Series; 2] = [Series::Basics, Series::Production];

    fn title(self) -> &'static str {
        match self {
            Series::Basics => tr("Use Ponzik"),
            Series::Production => tr("First productions"),
        }
    }

    fn icon(self) -> Icon {
        match self {
            Series::Basics => Icon::Layout,
            Series::Production => Icon::Wand,
        }
    }
}

pub struct Lesson {
    /// Stable id, saved in the settings.
    pub id: &'static str,
    pub series: Series,
    pub icon: Icon,
    pub title: &'static str,
    /// A few words under the title.
    pub blurb: &'static str,
    pub minutes: u32,
    /// Channels to edit, by name (the first found gets the focus).
    pub focus: &'static [&'static str],
    pub steps: &'static [Step],
}

const fn step(spot: Spot, text: &'static str, goal: Goal) -> Step {
    Step { spot, text, goal }
}

const BASS: &[&str] = &["Bass", "Reese bass", "808"];
const KEYS: &[&str] = &["Chords", "Piano", "Pad", "Melody"];

/// Every lesson, in the order of the panel.
pub const LESSONS: &[Lesson] = &[
    Lesson {
        id: "rack",
        series: Series::Basics,
        icon: Icon::Rack,
        title: tr_noop("The Channel Rack"),
        blurb: tr_noop("Steps, rows, your first loop"),
        minutes: 3,
        focus: &[],
        steps: &[
            step(Spot::TabRack, tr_noop("The Channel Rack: one row per sound."), Goal::View(View::Rack)),
            step(Spot::RackSteps, tr_noop("Click a few squares to switch steps on."), Goal::Notes(3)),
            step(Spot::Play, tr_noop("Press Space or Play to hear the loop."), Goal::Playing),
            step(Spot::RackSwing, tr_noop("Drag the swing to make it groove."), Goal::Swing),
        ],
    },
    Lesson {
        id: "piano",
        series: Series::Basics,
        icon: Icon::Piano,
        title: tr_noop("Piano roll basics"),
        blurb: tr_noop("Draw, stretch and erase notes"),
        minutes: 4,
        focus: &[],
        steps: &[
            step(Spot::TabPiano, tr_noop("The piano roll: the notes of one channel."), Goal::View(View::Piano)),
            step(Spot::PianoChannel, tr_noop("These arrows pick the channel you edit."), Goal::Next),
            step(Spot::PianoGrid, tr_noop("Click in the grid to draw a note."), Goal::Notes(1)),
            step(Spot::PianoGrid, tr_noop("Drag the end of a note to make it longer."), Goal::Longer),
            step(Spot::PianoGrid, tr_noop("Right-click a note to erase it."), Goal::Erase),
        ],
    },
    Lesson {
        id: "playlist",
        series: Series::Basics,
        icon: Icon::Playlist,
        title: tr_noop("Playlist and patterns"),
        blurb: tr_noop("Build the song from your loops"),
        minutes: 4,
        focus: &[],
        steps: &[
            step(
                Spot::TabPlaylist,
                tr_noop("The playlist: your song, from left to right."),
                Goal::View(View::Playlist),
            ),
            step(Spot::PlaylistPatterns, tr_noop("Pick a pattern in this list."), Goal::Next),
            step(Spot::PlaylistGrid, tr_noop("Click in a lane to place it."), Goal::Clips(1)),
            step(Spot::Mode, tr_noop("Switch to song mode to hear the whole song."), Goal::SongMode),
            step(Spot::Play, tr_noop("Press Play and listen."), Goal::Playing),
        ],
    },
    Lesson {
        id: "mixer",
        series: Series::Basics,
        icon: Icon::Mixer,
        title: tr_noop("Mixer and effects"),
        blurb: tr_noop("Levels, effects, the master"),
        minutes: 3,
        focus: &[],
        steps: &[
            step(Spot::TabMixer, tr_noop("The mixer: one strip per track."), Goal::View(View::Mixer)),
            step(Spot::MixerStrip, tr_noop("Drag a fader to set the volume."), Goal::Gain),
            step(Spot::MixerInserts, tr_noop("Click an empty slot to add an effect."), Goal::Effect),
            step(Spot::MasterStrip, tr_noop("Everything ends in the master, on the left."), Goal::Next),
        ],
    },
    Lesson {
        id: "sampler",
        series: Series::Basics,
        icon: Icon::Sample,
        title: tr_noop("Sampler and loops"),
        blurb: tr_noop("Drop a sample, make it play"),
        minutes: 3,
        focus: &[],
        steps: &[
            step(Spot::Browser, tr_noop("Open the browser."), Goal::Browser),
            step(Spot::BrowserTabs, tr_noop("Pick the samples tab."), Goal::SamplesTab),
            step(Spot::BrowserList, tr_noop("Click a sample to hear it."), Goal::Next),
            step(Spot::BrowserList, tr_noop("Drag it onto the Channel Rack or the playlist."), Goal::Sound),
        ],
    },
    Lesson {
        id: "record",
        series: Series::Basics,
        icon: Icon::Mic,
        title: tr_noop("Record your voice"),
        blurb: tr_noop("Input, arm, take"),
        minutes: 3,
        focus: &[],
        steps: &[
            step(Spot::Mic, tr_noop("Open the recording options, then the recording panel."), Goal::RecordPanel),
            step(Spot::MixerArm, tr_noop("Arm a track with its record dot."), Goal::Armed),
            step(Spot::Record, tr_noop("Switch recording on, then press Play."), Goal::Recording),
            step(Spot::Play, tr_noop("Stop when you are done: the take lands in the playlist."), Goal::Take),
        ],
    },
    Lesson {
        id: "export",
        series: Series::Basics,
        icon: Icon::Export,
        title: tr_noop("Export a track"),
        blurb: tr_noop("A file to share"),
        minutes: 2,
        focus: &[],
        steps: &[
            step(
                Spot::TabPlaylist,
                tr_noop("The export plays your playlist, start to end."),
                Goal::View(View::Playlist),
            ),
            step(Spot::Menu, tr_noop("Open the menu."), Goal::MenuOpen),
            step(Spot::Menu, tr_noop("In Export, pick WAV, or FLAC for a smaller file."), Goal::Exported),
        ],
    },
    Lesson {
        id: "beat",
        series: Series::Production,
        icon: Icon::Metronome,
        title: tr_noop("A beat in 5 minutes"),
        blurb: tr_noop("Kick, clap, hats, groove"),
        minutes: 5,
        focus: &[],
        steps: &[
            step(Spot::RackSteps, tr_noop("Kick on steps 1, 5, 9 and 13."), Goal::Notes(4)),
            step(Spot::RackSteps, tr_noop("Clap or snare on 5 and 13."), Goal::Notes(2)),
            step(Spot::RackSteps, tr_noop("Hats on every other step."), Goal::Notes(4)),
            step(Spot::Play, tr_noop("Play it and let it loop."), Goal::Playing),
            step(Spot::Tempo, tr_noop("Drag the tempo until it feels right."), Goal::Tempo),
            step(Spot::RackSwing, tr_noop("A little swing loosens it up."), Goal::Swing),
        ],
    },
    Lesson {
        id: "bassline",
        series: Series::Production,
        icon: Icon::Bass,
        title: tr_noop("A bassline in the scale"),
        blurb: tr_noop("Low notes that always fit"),
        minutes: 4,
        focus: BASS,
        steps: &[
            step(Spot::Scale, tr_noop("Pick a key and a scale here."), Goal::Scale),
            step(Spot::PianoSnap, tr_noop("Turn on scale snap: every note fits."), Goal::ScaleSnap),
            step(Spot::PianoGrid, tr_noop("Draw low notes on the root, one per beat."), Goal::Notes(4)),
            step(Spot::PianoGrid, tr_noop("Drag a few up or down for movement."), Goal::Next),
            step(Spot::Play, tr_noop("Play it with the beat."), Goal::Playing),
        ],
    },
    Lesson {
        id: "chords",
        series: Series::Production,
        icon: Icon::Chord,
        title: tr_noop("Chords and melody"),
        blurb: tr_noop("Harmony in a few clicks"),
        minutes: 5,
        focus: KEYS,
        steps: &[
            step(Spot::PianoChord, tr_noop("Turn on chord stamping."), Goal::ChordStamp(true)),
            step(Spot::PianoGrid, tr_noop("One click: a whole chord in the scale."), Goal::Notes(3)),
            step(Spot::PianoGrid, tr_noop("Add three more, one per bar."), Goal::Notes(6)),
            step(Spot::PianoChord, tr_noop("Turn stamping off for single notes."), Goal::ChordStamp(false)),
            step(Spot::PianoGrid, tr_noop("Draw a short melody above the chords."), Goal::Notes(4)),
        ],
    },
    Lesson {
        id: "drop",
        series: Series::Production,
        icon: Icon::Columns,
        title: tr_noop("Arrange a drop"),
        blurb: tr_noop("Intro, build-up, drop"),
        minutes: 5,
        focus: &[],
        steps: &[
            step(Spot::TabPlaylist, tr_noop("Open the playlist."), Goal::View(View::Playlist)),
            step(Spot::PlaylistGrid, tr_noop("Place your patterns over eight bars."), Goal::Clips(2)),
            step(Spot::Arrange, tr_noop("Click this lane to mark the sections."), Goal::Sections),
            step(Spot::PlaylistGrid, tr_noop("Leave one empty bar before the drop."), Goal::Next),
            step(Spot::Mode, tr_noop("Song mode, then play it from the top."), Goal::SongPlaying),
        ],
    },
    Lesson {
        id: "mix",
        series: Series::Production,
        icon: Icon::Speaker,
        title: tr_noop("Mix basics"),
        blurb: tr_noop("Balance, space, loudness"),
        minutes: 4,
        focus: &[],
        steps: &[
            step(Spot::MixerStrip, tr_noop("Set kick and bass first, then the rest around them."), Goal::Gain),
            step(Spot::MixerStrip, tr_noop("Pan hats and chords a little apart."), Goal::Pan),
            step(Spot::MasterInserts, tr_noop("Add a limiter on the master."), Goal::Effect),
            step(Spot::MasterStrip, tr_noop("Keep the master meter out of the red."), Goal::Next),
        ],
    },
];

pub fn lesson_index(id: &str) -> Option<usize> {
    LESSONS.iter().position(|l| l.id == id)
}

// --- Progress ---

/// What is saved: lessons completed, steps done in the others, the last one opened.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Progress {
    pub done: Vec<String>,
    /// Steps already done in lessons not finished yet.
    pub steps: Vec<(String, usize)>,
    pub current: Option<String>,
}

impl Progress {
    pub fn is_done(&self, id: &str) -> bool {
        self.done.iter().any(|d| d == id)
    }

    /// Steps done in a lesson (all of them once it is complete).
    pub fn steps_done(&self, i: usize) -> usize {
        let l = &LESSONS[i];
        if self.is_done(l.id) { l.steps.len() } else { self.steps.iter().find(|(id, _)| id == l.id).map_or(0, |s| s.1) }
    }

    fn set_step(&mut self, id: &str, n: usize) {
        self.steps.retain(|(i, _)| i != id);
        if n > 0 {
            self.steps.push((id.to_string(), n));
        }
    }

    fn complete(&mut self, id: &str) {
        self.steps.retain(|(i, _)| i != id);
        if !self.is_done(id) {
            self.done.push(id.to_string());
        }
        if self.current.as_deref() == Some(id) {
            self.current = None;
        }
    }

    pub fn is_empty(&self) -> bool {
        self.done.is_empty() && self.steps.is_empty() && self.current.is_none()
    }

    pub fn to_json(&self) -> Json {
        let mut fields =
            vec![("done".to_string(), Json::Arr(self.done.iter().map(|d| Json::Str(d.clone())).collect()))];
        if !self.steps.is_empty() {
            fields.push((
                "steps".into(),
                Json::Obj(self.steps.iter().map(|(id, n)| (id.clone(), (*n as f64).into())).collect()),
            ));
        }
        if let Some(c) = &self.current {
            fields.push(("current".into(), c.as_str().into()));
        }
        Json::Obj(fields)
    }

    /// Tolerant: unknown lessons are dropped, step counts clamped, the rest defaults.
    pub fn from_json(j: Option<&Json>) -> Self {
        let mut p = Progress::default();
        let Some(j) = j else { return p };
        for id in j.get("done").and_then(Json::as_arr).unwrap_or(&[]).iter().filter_map(Json::as_str) {
            if lesson_index(id).is_some() && !p.is_done(id) {
                p.done.push(id.to_string());
            }
        }
        for (id, n) in j.get("steps").and_then(Json::as_obj).unwrap_or(&[]) {
            let Some(i) = lesson_index(id) else { continue };
            let n = n.as_f64().filter(|v| v.is_finite()).unwrap_or(0.0).clamp(0.0, (LESSONS[i].steps.len() - 1) as f64)
                as usize;
            if !p.is_done(id) {
                p.set_step(id, n);
            }
        }
        p.current = j.get("current").and_then(Json::as_str).filter(|id| lesson_index(id).is_some()).map(str::to_string);
        p
    }
}

// --- Measures of the project, to see what the user did ---

#[derive(Clone, Copy, Debug, PartialEq)]
struct Metrics {
    notes: usize,
    note_len: u64,
    clips: usize,
    audio: usize,
    channels: usize,
    effects: usize,
    markers: usize,
    bpm: f32,
    swing: f32,
    key: u8,
    scale: ScaleKind,
    gains: f32,
    pans: f32,
}

fn metrics(p: &Project) -> Metrics {
    let notes = p.patterns.iter().flat_map(|pat| pat.notes.iter()).flatten();
    let (mut count, mut len) = (0, 0u64);
    for n in notes {
        count += 1;
        len += u64::from(n.len);
    }
    Metrics {
        notes: count,
        note_len: len,
        clips: p.clips.len(),
        audio: p.audio_clips.len(),
        channels: p.channels.len(),
        effects: p.tracks.iter().map(|t| t.inserts.iter().flatten().count()).sum(),
        markers: p.markers.len(),
        bpm: p.bpm,
        swing: p.swing,
        key: p.key,
        scale: p.scale,
        gains: p.tracks.iter().map(|t| t.gain).sum(),
        pans: p.tracks.iter().map(|t| t.pan).sum(),
    }
}

/// A lesson being followed.
pub struct Run {
    pub lesson: usize,
    pub step: usize,
    /// The project when the step started, and at the last frame.
    base: Metrics,
    prev: Metrics,
    /// When the step's goal was reached (the check shows, then the next step).
    done_at: Option<f32>,
    pub finished: bool,
}

/// State of the lessons: progress, the lesson followed, the panel.
#[derive(Default)]
pub struct Lessons {
    pub progress: Progress,
    pub run: Option<Run>,
    /// Where the current step's widget is this frame.
    anchor: Option<Rect>,
    /// The bubble drawn last frame: clicks there stay there.
    bubble: Option<Rect>,
    /// Series shown in the panel.
    pub tab: usize,
    /// An export happened during the step.
    exported: bool,
    /// Progress written to the settings (off in captures and tests).
    pub persist: bool,
    /// Steps never advance by themselves (captures).
    pub hold: bool,
}

impl Lessons {
    fn current_step(&self) -> Option<&'static Step> {
        let run = self.run.as_ref().filter(|r| !r.finished)?;
        LESSONS[run.lesson].steps.get(run.step)
    }

    pub fn active(&self) -> bool {
        self.run.is_some()
    }

    /// The current step waits for an export: the menu opens its Export submenu.
    pub fn wants_export(&self) -> bool {
        self.current_step().is_some_and(|st| st.goal == Goal::Exported)
    }

    /// Lessons done, out of all.
    pub fn count(&self) -> (usize, usize) {
        (LESSONS.iter().filter(|l| self.progress.is_done(l.id)).count(), LESSONS.len())
    }

    /// A lesson started and left before its end.
    pub fn unfinished(&self) -> bool {
        self.run.is_none() && self.progress.current.as_deref().is_some_and(|id| !self.progress.is_done(id))
    }
}

const BUBBLE_W: f32 = 360.0;

/// Which side of the bubble the arrow is on.
#[derive(Clone, Copy, PartialEq)]
enum Side {
    Top,
    Bottom,
    Left,
    Right,
}

/// Where the bubble goes: below the widget when there is room, inside large
/// areas (bottom right, away from what one clicks first), then beside, above.
fn place(a: Rect, w: f32, h: f32, screen: Rect) -> (f32, f32, Option<Side>) {
    let gap = 16.0;
    let cx = a.x + a.w * 0.5 - w * 0.5;
    let cy = a.y + a.h * 0.5 - h * 0.5;
    if a.bottom() + gap + h < screen.bottom() - 8.0 {
        (cx, a.bottom() + gap, Some(Side::Top))
    } else if a.h > 220.0 && a.w > w + 80.0 {
        (a.right() - w - 24.0, a.bottom() - h - 24.0, None)
    } else if a.right() + gap + w < screen.right() - 8.0 {
        (a.right() + gap, cy, Some(Side::Left))
    } else if a.y - gap - h > screen.y + 8.0 {
        (cx, a.y - gap - h, Some(Side::Bottom))
    } else {
        (a.x - gap - w, cy, Some(Side::Right))
    }
}

impl App {
    /// Where `s` was drawn this frame (called by the views; cheap when no lesson runs).
    pub(super) fn spot(&mut self, s: Spot, r: Rect) {
        if self.lessons.current_step().is_some_and(|st| st.spot == s) {
            self.lessons.anchor = Some(r);
        }
    }

    /// Opens the Learn panel, on the series of the lesson in progress.
    pub fn open_learn(&mut self) {
        if let Some(i) = self.lessons.progress.current.as_deref().and_then(lesson_index) {
            self.lessons.tab = Series::ALL.iter().position(|s| *s == LESSONS[i].series).unwrap_or(0);
        }
        self.open_popup(Popup::Learn, Rect::default());
    }

    /// Starts (or resumes) lesson `i` where it was left.
    pub fn start_lesson(&mut self, i: usize) {
        let Some(l) = LESSONS.get(i) else { return };
        self.popup = None;
        if self.tour.step.is_some() {
            self.tour.step = None;
            self.prefs.tour_done = true;
        }
        let at = if self.lessons.progress.is_done(l.id) {
            0
        } else {
            self.lessons.progress.steps_done(i).min(l.steps.len() - 1)
        };
        let m = metrics(&self.project);
        self.lessons.run = Some(Run { lesson: i, step: at, base: m, prev: m, done_at: None, finished: false });
        self.lessons.progress.current = Some(l.id.to_string());
        // The channel the lesson is about, when the project has one.
        if let Some(ch) = l.focus.iter().find_map(|name| self.project.channels.iter().position(|c| c.name == tr(name)))
        {
            self.focus = Focus::Channel(ch);
        }
        self.anim.set(wid("lesson-in", 0, 0), 0.0);
        self.go_to_lesson_step(at);
        self.save_lessons();
    }

    /// Stops the lesson; its progress stays.
    pub fn stop_lesson(&mut self) {
        self.lessons.run = None;
        self.lessons.bubble = None;
        self.lessons.anchor = None;
    }

    fn go_to_lesson_step(&mut self, step: usize) {
        let Some(run) = self.lessons.run.as_mut() else { return };
        let m = metrics(&self.project);
        (run.step, run.base, run.prev, run.done_at) = (step, m, m, None);
        self.lessons.exported = false;
        let st = &LESSONS[run.lesson].steps[step];
        // The widget comes into view, unless showing it is the step.
        if let Some(v) = st.spot.view()
            && !matches!(st.goal, Goal::View(_))
            && self.view_shown(v)
        {
            self.set_view(v);
        }
        if st.spot.in_browser() {
            self.sidebar = true;
        }
        self.anim.set(wid("lesson-step", 0, 0), 0.0);
    }

    fn advance_lesson(&mut self) {
        let Some(run) = self.lessons.run.as_mut() else { return };
        let l = &LESSONS[run.lesson];
        let next = run.step + 1;
        if next >= l.steps.len() {
            run.finished = true;
            self.lessons.progress.complete(l.id);
            self.anim.set(wid("lesson-done", 0, 0), 0.0);
        } else {
            self.lessons.progress.set_step(l.id, next);
            self.go_to_lesson_step(next);
        }
        self.save_lessons();
    }

    fn save_lessons(&mut self) {
        if self.lessons.persist {
            self.save_settings();
        }
    }

    /// An export was made (the export lesson waits for it).
    pub(super) fn lesson_exported(&mut self) {
        self.lessons.exported = true;
    }

    fn goal_met(&self, goal: Goal, base: &Metrics, prev: &Metrics, now: &Metrics) -> bool {
        let popup_is = |f: fn(&Popup) -> bool| self.popup.as_ref().is_some_and(|p| f(&p.kind));
        match goal {
            Goal::Next => false,
            Goal::View(v) => self.view == v,
            Goal::Playing => self.playing,
            Goal::SongMode => self.project.mode == ponzik_engine::PlayMode::Song,
            Goal::SongPlaying => self.playing && self.project.mode == ponzik_engine::PlayMode::Song,
            Goal::Notes(n) => now.notes >= base.notes + n,
            Goal::Longer => now.notes == prev.notes && now.note_len > prev.note_len,
            Goal::Erase => now.notes < base.notes,
            Goal::Clips(n) => now.clips >= base.clips + n,
            Goal::Sound => now.channels > base.channels || now.audio > base.audio,
            Goal::Take => now.audio > base.audio,
            Goal::Effect => now.effects > base.effects,
            Goal::Gain => now.gains != base.gains,
            Goal::Pan => now.pans != base.pans,
            Goal::Tempo => now.bpm != base.bpm,
            Goal::Swing => now.swing != base.swing,
            Goal::Scale => (now.key, now.scale) != (base.key, base.scale),
            Goal::ScaleSnap => self.piano.scale_snap,
            Goal::ChordStamp(on) => self.piano.chord_stamp == on,
            Goal::Sections => now.markers > base.markers,
            Goal::Browser => self.sidebar,
            Goal::SamplesTab => self.sidebar && self.sidebar_tab == 2,
            Goal::RecordPanel => self.rec.panel,
            Goal::Armed => !self.rec.armed.is_empty(),
            Goal::Recording => self.record && self.playing,
            Goal::MenuOpen => popup_is(|p| matches!(p, Popup::MainMenu)),
            Goal::Exported => self.lessons.exported,
        }
    }

    /// Each frame: has the user done what the step asks?
    pub(super) fn lesson_tick(&mut self) {
        let Some(run) = self.lessons.run.as_ref().filter(|r| !r.finished) else { return };
        let goal = LESSONS[run.lesson].steps[run.step].goal;
        let now = metrics(&self.project);
        let reached = run.done_at.is_none() && self.goal_met(goal, &run.base, &run.prev, &now);
        let due = run.done_at.is_some_and(|t| self.time - t > 0.9) && !self.lessons.hold;
        let time = self.time;
        if let Some(run) = self.lessons.run.as_mut() {
            run.prev = now;
            if reached {
                run.done_at = Some(time);
                self.anim.set(wid("lesson-check", 0, 0), 0.0);
            }
        }
        if due {
            self.advance_lesson();
        }
    }

    /// A popup covers the screen: no bubble this frame.
    pub(super) fn lesson_hidden(&mut self) {
        self.lessons.anchor = None;
        self.lessons.bubble = None;
    }

    /// True when the mouse is over the lesson bubble.
    pub(super) fn over_lesson(&self, input: &Input) -> bool {
        self.lessons.bubble.is_some_and(|r| r.contains(input.x, input.y))
    }

    /// The bubble of the current step and the pulse around its widget; drawn above the views.
    pub(super) fn lesson_layer(&mut self, c: &mut Canvas, input: &Input, screen: Rect) {
        let anchor = self.lessons.anchor.take();
        let Some(run) = self.lessons.run.as_ref() else {
            self.lessons.bubble = None;
            return;
        };
        if run.finished {
            let lesson = run.lesson;
            self.lesson_done_card(c, input, screen, lesson);
            return;
        }
        let (li, si, done_at) = (run.lesson, run.step, run.done_at);
        let lesson = &LESSONS[li];
        let st = &lesson.steps[si];
        let t = self.theme;
        let appear = self.anim.to(wid("lesson-in", 0, 0), 1.0, T_LAYOUT);
        let fresh = self.anim.to(wid("lesson-step", 0, 0), 1.0, T_LAYOUT);
        let reached = done_at.is_some();
        let check = self.anim.to(wid("lesson-check", 0, 0), reached as u8 as f32, T_LAYOUT);

        // Size: header, the sentence (big), the dots and the button.
        let pad = 18.0;
        let text = tr(st.text);
        let lines = self.wrap(self.f.sans_med, 16.0, text, BUBBLE_W - 2.0 * pad - 4.0);
        let h = (pad + 24.0 + 12.0 + lines.len() as f32 * 23.0 + 16.0 + 32.0 + pad).round();
        let w = BUBBLE_W;
        let (x, y, side) = match anchor {
            Some(a) => place(a, w, h, screen),
            None => (screen.x + (screen.w - w) * 0.5, screen.bottom() - h - 120.0, None),
        };
        let x = x.clamp(screen.x + 12.0, screen.right() - w - 12.0);
        let y = y.clamp(screen.y + 56.0, screen.bottom() - h - 12.0);
        // The bubble glides from one widget to the next.
        let x = self.anim.to(wid("lesson-x", 0, 0), x, T_LAYOUT).round();
        let y = (self.anim.to(wid("lesson-y", 0, 0), y, T_LAYOUT) + (1.0 - appear) * 8.0).round();
        let r = Rect::new(x, y, w, h);
        self.lessons.bubble = Some(r);

        // The widget: an accent ring breathing, and a ripple going out from it.
        if let Some(a) = anchor {
            let breath = 0.5 + 0.5 * (self.time * 3.0).sin();
            let ring = a.inset(-4.0);
            c.stroke_rrect(ring, 12.0, 2.0, t.accent.alpha((0.55 + 0.4 * breath) * appear * (1.0 - check * 0.5)));
            let ph = (self.time / 1.6).fract();
            if !reached {
                c.stroke_rrect(
                    ring.inset(-10.0 * ph),
                    12.0 + 10.0 * ph,
                    2.0 * (1.0 - ph) + 0.5,
                    t.accent.alpha(0.45 * (1.0 - ph) * appear),
                );
            }
        }

        // Card and arrow.
        c.fill_rrect(r.translate(0.0, 8.0).inset(-2.0), 18.0, t.shade(90.0 * appear));
        c.fill_rrect(r, 16.0, t.raised.alpha(appear));
        c.stroke_rrect(r, 16.0, 1.0 + check, t.accent.alpha((0.4 + 0.5 * check) * appear));
        if let (Some(a), Some(side)) = (anchor, side) {
            let mut tri = Path::new();
            match side {
                Side::Top | Side::Bottom => {
                    let ax = (a.x + a.w * 0.5).clamp(r.x + 26.0, r.right() - 26.0);
                    let (base, tip) =
                        if side == Side::Top { (r.y + 1.0, r.y - 9.0) } else { (r.bottom() - 1.0, r.bottom() + 9.0) };
                    tri.polygon(&[(ax - 10.0, base), (ax, tip), (ax + 10.0, base)]);
                }
                Side::Left | Side::Right => {
                    let ay = (a.y + a.h * 0.5).clamp(r.y + 26.0, r.bottom() - 26.0);
                    let (base, tip) =
                        if side == Side::Left { (r.x + 1.0, r.x - 9.0) } else { (r.right() - 1.0, r.right() + 9.0) };
                    tri.polygon(&[(base, ay - 10.0), (tip, ay), (base, ay + 10.0)]);
                }
            }
            c.fill_path(&tri, t.raised.alpha(appear));
        }

        // Header: the lesson's icon and title, the close button.
        let hy = r.y + pad + 12.0;
        let tile = Rect::new(r.x + pad, hy - 12.0, 24.0, 24.0);
        c.fill_rrect(tile, 7.0, t.accent.alpha(0.16 * appear));
        draw_icon(c, lesson.icon, tile.x + 12.0, hy, 14.0, t.accent.alpha(appear));
        let title = tr(lesson.title);
        self.text_fit(
            c,
            self.f.sans_med,
            12.0,
            tile.right() + 10.0,
            hy,
            title,
            w - 2.0 * pad - 70.0,
            t.text2.alpha(appear),
        );
        let close_r = Rect::new(r.right() - pad - 26.0 + 6.0, hy - 13.0, 26.0, 26.0);
        let stop = self.icon_button(
            c,
            input,
            wid("lesson-close", 0, 0),
            close_r,
            Icon::Close,
            false,
            (tr("Stop the lesson"), tr("Your progress is kept")),
        );

        // The sentence, rising in at each step; a check pops when it is done.
        let mut ty = r.y + pad + 24.0 + 12.0 + 11.0 + (1.0 - fresh) * 6.0;
        for line in &lines {
            self.text(c, self.f.sans_med, 16.0, r.x + pad, ty, line, t.text.alpha(appear * fresh));
            ty += 23.0;
        }

        // Dots: one per step, the current one long.
        let by = r.bottom() - pad - 16.0;
        let n = lesson.steps.len();
        let mut dx = r.x + pad;
        for k in 0..n {
            let cur = k == si;
            let wk = if cur { 18.0 } else { 7.0 };
            let col = if k < si || (cur && reached) {
                t.accent
            } else if cur {
                t.accent.alpha(0.75)
            } else {
                t.muted.alpha(0.45)
            };
            c.fill_rrect(Rect::new(dx, by - 3.5, wk, 7.0), 3.5, col.alpha(appear));
            dx += wk + 6.0;
        }
        let count = format!("{} / {}", si + 1, n);
        self.text(c, self.f.mono, 11.0, dx + 4.0, by, &count, t.muted.alpha(appear));

        // Next (the main button when the step only reads), or the check once done.
        let mut next = false;
        if check > 0.01 {
            let s = 0.6 + 0.4 * check;
            let (kx, ky) = (r.right() - pad - 16.0, by);
            c.fill_circle(kx, ky, 16.0 * s, t.accent.alpha(check * appear));
            draw_icon(c, Icon::Check, kx, ky, 18.0 * s, t.on_accent.alpha(check * appear));
        }
        if check < 0.99 {
            let manual = st.goal == Goal::Next;
            let label = if si + 1 == n { tr("Finish") } else { tr("Next") };
            let bw = self.fonts.measure(self.f.sans_med, 13.0, label) + 50.0;
            let br = Rect::new(r.right() - pad - bw, by - 16.0, bw, 32.0);
            let tip = if manual { "" } else { tr("Skip this step") };
            if !reached {
                next = self.lab_button(
                    c,
                    input,
                    wid("lesson-next", li, si),
                    br,
                    Some(Icon::ChevronRight),
                    label,
                    if manual { 3 } else { 0 },
                    tip,
                );
            }
        }
        if stop {
            self.stop_lesson();
        } else if next {
            self.advance_lesson();
        }
    }

    /// The end of a lesson: a check, the next lesson, the panel.
    fn lesson_done_card(&mut self, c: &mut Canvas, input: &Input, screen: Rect, li: usize) {
        let t = self.theme;
        let open = self.anim.to(wid("lesson-done", 0, 0), 1.0, T_LAYOUT * 1.5);
        let (w, h) = (BUBBLE_W + 40.0, 196.0);
        let r = Rect::new(
            ((screen.w - w) * 0.5).round(),
            (screen.bottom() - h - 110.0 + (1.0 - open) * 14.0).round(),
            w,
            h,
        );
        self.lessons.bubble = Some(r);
        c.fill_rrect(r.translate(0.0, 10.0).inset(-3.0), 20.0, t.shade(100.0 * open));
        c.fill_rrect(r, 18.0, t.raised.alpha(open));
        c.stroke_rrect(r, 18.0, 1.5, t.accent.alpha(0.7 * open));

        // The check, and a ring of dots flying out once.
        let (cx, cy) = (r.x + r.w * 0.5, r.y + 46.0);
        for k in 0..10 {
            let a = k as f32 * std::f32::consts::TAU / 10.0 - 1.2;
            let d = 26.0 + 18.0 * open;
            let col = if k % 2 == 0 { t.accent } else { t.accent_hot };
            c.fill_circle(
                cx + a.cos() * d,
                cy + a.sin() * d,
                2.6 * (1.0 - open * 0.6),
                col.alpha((1.0 - open) * 0.9 + 0.1),
            );
        }
        c.fill_circle(cx, cy, 24.0 * (0.7 + 0.3 * open), t.accent.alpha(open));
        draw_icon(c, Icon::Check, cx, cy, 24.0, t.on_accent.alpha(open));
        self.text_center(c, self.f.sans_med, 18.0, cx, r.y + 94.0, tr("Lesson complete"), t.text.alpha(open));
        let sub = self.fit(self.f.sans, 13.0, tr(LESSONS[li].title), w - 40.0);
        self.text_center(c, self.f.sans, 13.0, cx, r.y + 118.0, &sub, t.text2.alpha(open));

        // The next lesson not done yet, after this one in the panel's order.
        let next = (1..LESSONS.len())
            .map(|k| (li + k) % LESSONS.len())
            .find(|&k| !self.lessons.progress.is_done(LESSONS[k].id));
        let by = r.bottom() - 22.0 - 32.0;
        let all = tr("All lessons");
        let aw = self.fonts.measure(self.f.sans_med, 13.0, all) + 50.0;
        let all_r = Rect::new(r.x + 22.0, by, aw, 32.0);
        let to_panel = self.lab_button(c, input, wid("lesson-all", 0, 0), all_r, Some(Icon::Learn), all, 0, "");
        let (label, tip) = match next {
            Some(k) => (tr("Next lesson"), tr(LESSONS[k].title)),
            None => (tr("Done"), ""),
        };
        let nw = self.fonts.measure(self.f.sans_med, 13.0, label) + 50.0;
        let next_r = Rect::new(r.right() - 22.0 - nw, by, nw, 32.0);
        let go = self.lab_button(
            c,
            input,
            wid("lesson-go", 0, 0),
            next_r,
            Some(if next.is_some() { Icon::ChevronRight } else { Icon::Check }),
            label,
            3,
            tip,
        );
        if to_panel {
            self.stop_lesson();
            self.open_learn();
        } else if go {
            self.stop_lesson();
            if let Some(k) = next {
                self.start_lesson(k);
            }
        }
    }

    /// The Learn panel: two series of lesson cards. False when it closes.
    pub(super) fn learn_popup(&mut self, c: &mut Canvas, input: &Input, screen: Rect) -> bool {
        let t = self.theme;
        let open = self.anim.to(wid("popup", 0, 0), 1.0, T_HOVER);
        c.fill_rect(screen, t.shade(170.0 * open));
        let (cols, card_h, gap) = (3usize, 132.0, 14.0);
        let rows = Series::ALL
            .iter()
            .map(|s| LESSONS.iter().filter(|l| l.series == *s).count().div_ceil(cols))
            .max()
            .unwrap_or(1);
        let w = 920.0f32.min(screen.w - 40.0);
        let h = (176.0 + rows as f32 * (card_h + gap) - gap + 64.0).min(screen.h - 40.0);
        let r = Rect::new(((screen.w - w) * 0.5).round(), ((screen.h - h) * 0.5 + (1.0 - open) * 12.0).round(), w, h);
        c.fill_rrect(r.translate(0.0, 12.0).inset(-4.0), 26.0, t.shade(100.0 * open));
        c.fill_rrect(r, 22.0, t.panel);
        c.stroke_rrect(r, 22.0, 1.0, t.hairline);
        let mut keep = true;

        // Header: the cap, the title, the progress ring, close.
        let mark = Rect::new(r.x + 28.0, r.y + 26.0, 56.0, 56.0);
        c.fill_rrect(mark, 18.0, t.accent);
        draw_icon(c, Icon::Learn, mark.x + 28.0, mark.y + 28.0, 32.0, t.on_accent);
        let tx = mark.right() + 18.0;
        self.text(c, self.f.sans_med, 22.0, tx, r.y + 44.0, tr("Learn"), t.text);
        self.text_fit(
            c,
            self.f.sans,
            13.0,
            tx,
            r.y + 68.0,
            tr("Short hands-on lessons, in your project. Stop whenever you like."),
            r.right() - tx - 170.0,
            t.text2,
        );
        let close_r = Rect::new(r.right() - 20.0 - 34.0, r.y + 20.0, 34.0, 34.0);
        if self.icon_button(c, input, wid("learn-close", 0, 0), close_r, Icon::Close, false, (tr("Close"), tr("Esc"))) {
            keep = false;
        }
        let (done, total) = self.lessons.count();
        let (rx, ry) = (close_r.x - 44.0, r.y + 54.0);
        let frac = self.anim.to(wid("learn-ring", 0, 0), done as f32 / total.max(1) as f32, T_LAYOUT * 2.0);
        c.stroke_circle(rx, ry, 20.0, 4.0, t.well);
        if frac > 0.001 {
            let a0 = -std::f32::consts::FRAC_PI_2;
            c.stroke_arc(rx, ry, 20.0, a0, a0 + frac * std::f32::consts::TAU, 4.0, t.accent);
        }
        self.text_center(c, self.f.mono_med, 12.0, rx, ry, &format!("{}/{}", done, total), t.text);
        self.tip_zone(input, Rect::new(rx - 22.0, ry - 22.0, 44.0, 44.0), tr("Lessons completed"), "");

        // Series tabs: big pills with an icon and a count.
        let tab_y = r.y + 104.0;
        let mut x = r.x + 28.0;
        for (k, s) in Series::ALL.iter().enumerate() {
            let ids: Vec<usize> = (0..LESSONS.len()).filter(|&i| LESSONS[i].series == *s).collect();
            let n_done = ids.iter().filter(|&&i| self.lessons.progress.is_done(LESSONS[i].id)).count();
            let label = s.title();
            let count = format!("{}/{}", n_done, ids.len());
            let tw =
                self.fonts.measure(self.f.sans_med, 14.0, label) + self.fonts.measure(self.f.mono, 11.0, &count) + 74.0;
            let tab = Rect::new(x, tab_y, tw, 44.0);
            let id = wid("learn-tab", k, 0);
            let hov = self.hovered(input, id, tab);
            let hv = self.hover_anim(id, hov);
            let on = self.anim.to(id ^ 0x4f4e, (self.lessons.tab == k) as u8 as f32, T_HOVER);
            c.fill_rrect(tab, 22.0, t.bg_deep.mix(t.raised, hv * 0.8).mix(t.raised_hi, on));
            c.stroke_rrect(tab, 22.0, 1.0, t.hairline.mix(t.accent, on * 0.8));
            let ink = t.text2.mix(t.text, hv.max(on));
            draw_icon(c, s.icon(), tab.x + 24.0, tab.y + 22.0, 17.0, t.muted.mix(t.accent, on.max(hv * 0.5)));
            let lx = self.text(c, self.f.sans_med, 14.0, tab.x + 42.0, tab.y + 22.0, label, ink);
            let chip =
                Rect::new(tab.x + 42.0 + lx + 8.0, tab.y + 12.0, tab.right() - 14.0 - (tab.x + 42.0 + lx + 8.0), 20.0);
            c.fill_rrect(chip, 10.0, t.well.mix(t.accent, 0.25 * on));
            self.text_center(
                c,
                self.f.mono,
                11.0,
                chip.x + chip.w * 0.5,
                chip.y + 10.0,
                &count,
                t.text2.mix(t.accent, on),
            );
            if hov {
                self.set_cursor(ponzik_window::Cursor::Hand);
                if input.pressed {
                    self.lessons.tab = k;
                }
            }
            x += tw + 10.0;
        }

        // Cards of the series.
        let series = Series::ALL[self.lessons.tab.min(Series::ALL.len() - 1)];
        let grid = Rect::new(r.x + 28.0, r.y + 172.0, r.w - 56.0, r.h - 172.0 - 56.0);
        let cw = ((grid.w - gap * (cols as f32 - 1.0)) / cols as f32).floor();
        let shown: Vec<usize> = (0..LESSONS.len()).filter(|&i| LESSONS[i].series == series).collect();
        let mut start = None;
        for (k, &i) in shown.iter().enumerate() {
            let cell = Rect::new(
                grid.x + (k % cols) as f32 * (cw + gap),
                grid.y + (k / cols) as f32 * (card_h + gap),
                cw,
                card_h,
            );
            // Cards come in one after the other.
            let enter =
                self.anim.to(wid("learn-card-in", i, self.lessons.tab), 1.0, T_LAYOUT * (1.0 + k as f32 * 0.35));
            if self.lesson_card(c, input, cell.translate(0.0, (1.0 - enter) * 10.0), i, enter) {
                start = Some(i);
            }
        }

        // Footer: lessons edit the project in front of you.
        let fy = r.bottom() - 30.0;
        draw_icon(c, Icon::Undo, r.x + 38.0, fy, 13.0, t.muted);
        let undo = self.key_label(super::keymap::Shortcut::Undo);
        self.text_fit(
            c,
            self.f.sans,
            12.0,
            r.x + 54.0,
            fy,
            &tr_fmt!("Lessons work on the open project: {} undoes any change.", undo),
            r.w - 90.0,
            t.muted,
        );

        if let Some(i) = start {
            self.start_lesson(i);
            return false;
        }
        if input.pressed && !r.contains(input.x, input.y) {
            keep = false;
        }
        keep
    }

    /// One card; true when clicked.
    fn lesson_card(&mut self, c: &mut Canvas, input: &Input, cell: Rect, i: usize, enter: f32) -> bool {
        let t = self.theme;
        let l = &LESSONS[i];
        let id = wid("learn-card", i, 0);
        let hov = self.hovered(input, id, cell);
        let hv = self.hover_anim(id, hov);
        let press = self.anim.to(id ^ 0x5052, if hov && input.down { 0.97 } else { 1.0 }, T_PRESS);
        let done = self.lessons.progress.is_done(l.id);
        let steps = self.lessons.progress.steps_done(i);
        let resume = !done && steps > 0;
        let r = cell.translate(0.0, -3.0 * hv).scale(press);
        if hv > 0.01 {
            c.fill_rrect(r.translate(0.0, 6.0), 16.0, t.shade(70.0 * hv * enter));
        }
        c.fill_rrect(r, 16.0, t.raised.mix(t.raised_hi, hv * 0.6).alpha(enter));
        let edge = if done { 0.35 } else { 0.0 };
        c.stroke_rrect(r, 16.0, 1.0 + hv, t.hairline.mix(t.accent, (hv * 0.8).max(edge)).alpha(enter));

        // Icon tile; a check badge on its corner once done.
        let tile = Rect::new(r.x + 16.0, r.y + 16.0, 52.0, 52.0);
        c.fill_rrect(tile, 15.0, t.well.mix(t.accent, 0.14 + 0.16 * hv).alpha(enter));
        draw_icon(c, l.icon, tile.x + 26.0, tile.y + 26.0, 26.0 + 2.0 * hv, t.accent.alpha(enter));
        if done {
            let (bx, by) = (tile.right() - 2.0, tile.y + 2.0);
            c.fill_circle(bx, by, 10.5, t.raised.mix(t.raised_hi, hv * 0.6));
            c.fill_circle(bx, by, 8.5, t.accent.alpha(enter));
            draw_icon(c, Icon::Check, bx, by, 11.0, t.on_accent.alpha(enter));
        }
        let tx = tile.right() + 14.0;
        self.text_fit(
            c,
            self.f.sans_med,
            15.0,
            tx,
            r.y + 32.0,
            tr(l.title),
            r.right() - tx - 14.0,
            t.text.alpha(enter),
        );
        // The blurb on two lines at most.
        let max_w = r.right() - tx - 14.0;
        let mut lines = self.wrap(self.f.sans, 12.0, tr(l.blurb), max_w);
        if lines.len() > 2 {
            lines[1] = lines[1..].join(" ");
            lines.truncate(2);
        }
        for (k, line) in lines.iter().enumerate() {
            self.text_fit(c, self.f.sans, 12.0, tx, r.y + 53.0 + k as f32 * 16.0, line, max_w, t.text2.alpha(enter));
        }

        // Bottom: a dot per step, the duration (or what a click does).
        let by = r.bottom() - 24.0;
        let n = l.steps.len();
        for k in 0..n {
            let on = k < steps;
            let col = if on { t.accent } else { t.muted.alpha(0.35) };
            c.fill_circle(r.x + 22.0 + k as f32 * 13.0, by, 3.5, col.alpha(enter));
        }
        let rest = 1.0 - hv;
        if rest > 0.01 {
            let label = tr_fmt!("{} min", l.minutes);
            let lw = self.fonts.measure(self.f.sans, 12.0, &label);
            self.text(c, self.f.sans, 12.0, r.right() - 16.0 - lw, by, &label, t.muted.alpha(rest * enter));
            draw_icon(c, Icon::History, r.right() - 16.0 - lw - 12.0, by, 12.0, t.muted.alpha(rest * enter));
        }
        if hv > 0.01 {
            let label = if done {
                tr("Replay")
            } else if resume {
                tr("Resume")
            } else {
                tr("Start")
            };
            let lw = self.fonts.measure(self.f.sans_med, 13.0, label);
            let x = r.right() - 16.0 - lw - 22.0 + (1.0 - hv) * 8.0;
            let pill = Rect::new(x - 12.0, by - 14.0, lw + 46.0, 28.0);
            c.fill_rrect(pill, 14.0, t.accent.alpha(hv * enter));
            draw_icon(c, Icon::Play, x + 4.0, by, 12.0, t.on_accent.alpha(hv));
            self.text(c, self.f.sans_med, 13.0, x + 16.0, by, label, t.on_accent.alpha(hv));
        } else if resume {
            self.text(
                c,
                self.f.sans_med,
                11.0,
                r.x + 22.0 + n as f32 * 13.0 + 4.0,
                by,
                tr("In progress"),
                t.accent.alpha(enter),
            );
        }
        if hov {
            self.set_cursor(ponzik_window::Cursor::Hand);
        }
        hov && input.pressed
    }

    /// Screenshot setup, `PONZIK_LESSON`: `list[:series]` opens the panel,
    /// `<id>:<step>[:ok]` shows that step (done), `<id>:done` the end of the lesson.
    /// `PONZIK_LESSONS_DONE=rack,beat,piano:2` fills in some progress.
    pub(super) fn screenshot_lesson(&mut self, spec: &str, done: Option<&str>) {
        self.lessons.hold = true;
        for part in done.unwrap_or("").split(',').map(str::trim).filter(|p| !p.is_empty()) {
            match part.split_once(':') {
                Some((id, n)) => {
                    if let (Some(i), Ok(n)) = (lesson_index(id), n.parse::<usize>()) {
                        self.lessons.progress.set_step(LESSONS[i].id, n.min(LESSONS[i].steps.len() - 1));
                        self.lessons.progress.current = Some(LESSONS[i].id.to_string());
                    }
                }
                None if lesson_index(part).is_some() => self.lessons.progress.complete(part),
                None => {}
            }
        }
        let (name, arg) = spec.split_once(':').unwrap_or((spec, ""));
        if name == "list" {
            self.lessons.tab = arg.parse::<usize>().unwrap_or(0).min(Series::ALL.len() - 1);
            self.open_popup(Popup::Learn, Rect::default());
            return;
        }
        let Some(i) = lesson_index(name) else {
            eprintln!(
                "PONZIK_LESSON: unknown lesson {name} (list, {})",
                LESSONS.iter().map(|l| l.id).collect::<Vec<_>>().join(", ")
            );
            return;
        };
        self.start_lesson(i);
        if arg == "done" {
            if let Some(run) = self.lessons.run.as_mut() {
                run.finished = true;
            }
            self.lessons.progress.complete(LESSONS[i].id);
        } else {
            // `<id>:<step>:ok`: the step's action just done (the check shows).
            let (step, ok) = arg.split_once(':').map_or((arg, false), |(s, k)| (s, k == "ok"));
            let step = step.parse::<usize>().unwrap_or(0).min(LESSONS[i].steps.len() - 1);
            self.go_to_lesson_step(step);
            if let Some(run) = self.lessons.run.as_mut().filter(|_| ok) {
                run.done_at = Some(self.time);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lessons_are_well_formed() {
        let mut ids: Vec<&str> = LESSONS.iter().map(|l| l.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), LESSONS.len(), "lesson ids must be unique");
        assert!((8..=14).contains(&LESSONS.len()));
        for s in Series::ALL {
            assert!(LESSONS.iter().any(|l| l.series == s), "{s:?} has no lesson");
        }
        for l in LESSONS {
            assert!((3..=7).contains(&l.steps.len()), "{}: {} steps", l.id, l.steps.len());
            assert!((1..=10).contains(&l.minutes), "{}", l.id);
            for st in l.steps {
                assert!(Spot::ALL.contains(&st.spot), "{}: {:?} is not in Spot::ALL", l.id, st.spot);
                let words = st.text.split_whitespace().count();
                assert!(words <= 15, "{}: \"{}\" has {} words (15 at most)", l.id, st.text, words);
                // A step that waits for a view points at that view's tab.
                if let Goal::View(v) = st.goal {
                    let tab = match v {
                        View::Rack => Spot::TabRack,
                        View::Piano => Spot::TabPiano,
                        View::Playlist => Spot::TabPlaylist,
                        View::Mixer => Spot::TabMixer,
                        _ => panic!("{}: no tab spot for {v:?}", l.id),
                    };
                    assert_eq!(st.spot, tab, "{}", l.id);
                }
            }
        }
    }

    /// Every spot is reported by some view (`self.spot(Spot::X, rect)`, or a
    /// `Spot::X` picked before the call, as for the view tabs).
    #[test]
    fn every_spot_is_drawn_somewhere() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/gui");
        let mut src = String::new();
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "rs") && path.file_name().is_some_and(|n| n != "lessons.rs") {
                src.push_str(&std::fs::read_to_string(&path).unwrap());
            }
        }
        for s in Spot::ALL {
            let name = format!("Spot::{s:?}");
            let found = src
                .match_indices(&name)
                .any(|(i, _)| !src[i + name.len()..].starts_with(|c: char| c.is_alphanumeric()));
            assert!(found, "no view reports {s:?} (expected `self.spot({name}, rect)`)");
        }
    }

    #[test]
    fn lesson_texts_are_translated() {
        let fr = ponzik_i18n::Lang::from_code("fr").expect("French catalog");
        let cat = fr.catalog().expect("French catalog");
        let mut texts: Vec<&str> = Vec::new();
        for l in LESSONS {
            texts.extend([l.title, l.blurb]);
            texts.extend(l.steps.iter().map(|s| s.text));
        }
        for text in texts {
            assert!(cat.get(text).is_some(), "no French translation for \"{text}\"");
        }
    }

    #[test]
    fn progress_round_trip() {
        let p = Progress {
            done: vec!["rack".into(), "beat".into()],
            steps: vec![("piano".into(), 2), ("mix".into(), 1)],
            current: Some("piano".into()),
        };
        let text = p.to_json().pretty();
        let back = Progress::from_json(Some(&ponzik_project::json::parse(&text).unwrap()));
        assert_eq!(back, p);
        assert_eq!(back.steps_done(lesson_index("piano").unwrap()), 2);
        assert_eq!(back.steps_done(lesson_index("rack").unwrap()), LESSONS[0].steps.len());
        assert_eq!(Progress::from_json(Some(&Progress::default().to_json())), Progress::default());
    }

    #[test]
    fn progress_loading_is_tolerant() {
        let j = ponzik_project::json::parse(
            r#"{"done": ["rack", "gone", 3, "rack"], "steps": {"piano": 99, "nope": 1, "mix": "x", "rack": 2}, "current": "nowhere"}"#,
        )
        .unwrap();
        let p = Progress::from_json(Some(&j));
        assert_eq!(p.done, vec!["rack".to_string()]);
        let piano = lesson_index("piano").unwrap();
        assert_eq!(p.steps, vec![("piano".to_string(), LESSONS[piano].steps.len() - 1)]);
        assert_eq!(p.current, None);
        assert_eq!(Progress::from_json(None), Progress::default());
        assert_eq!(Progress::from_json(Some(&Json::Num(4.0))), Progress::default());
    }

    #[test]
    fn bubbles_stay_by_their_widget() {
        let screen = Rect::new(0.0, 0.0, 1600.0, 1000.0);
        // A button of the top bar: below it.
        let (_, y, side) = place(Rect::new(800.0, 12.0, 40.0, 36.0), 360.0, 160.0, screen);
        assert!(side == Some(Side::Top) && y > 48.0);
        // A large grid down to the bottom: inside it.
        let (x, y, side) = place(Rect::new(300.0, 120.0, 1200.0, 860.0), 360.0, 160.0, screen);
        assert!(side.is_none() && x > 300.0 && y + 160.0 < 980.0);
        // A tall narrow strip: beside it.
        let (x, _, side) = place(Rect::new(200.0, 120.0, 90.0, 860.0), 360.0, 160.0, screen);
        assert!(side == Some(Side::Left) && x >= 290.0);
    }
}
