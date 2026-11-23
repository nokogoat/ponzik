//! Plugin editors: the plugin's own interface (CLAP `clap.gui`, VST 3
//! `IPlugView`) embedded into a host window, as the app sees it.
//!
//! An instance hands out its editor as an [`Editor`] before being sent to the
//! engine; every method is called from the UI (main) thread only. The editor
//! keeps working while the instance renders on the audio thread, and turns
//! inert ([`PluginEditor::is_alive`] false) once the instance is destroyed.
//!
//! [`RunLoop`] is the main-thread event loop that Linux plugin GUIs drive their
//! own work from: timers (`clap.timer-support`, `ITimerHandler`) and file
//! descriptors (`clap.posix-fd-support`, `IEventHandler`), pumped by
//! [`PluginEditor::idle`].
//!
//! The editor is also the main-thread handle on the plugin's opaque state
//! ([`PluginEditor::save_state`], [`PluginEditor::load_state`]): CLAP
//! `clap.state`, VST 3 component and controller states.

use std::sync::Arc;
use std::time::{Duration, Instant};

/// Native window an editor is embedded into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParentWindow {
    /// X11 window id.
    X11(u32),
    /// `HWND`.
    Win32(usize),
    /// `NSView*`.
    Cocoa(usize),
}

impl ParentWindow {
    /// CLAP window API name (`clap_window.api`).
    pub fn clap_api(&self) -> &'static str {
        match self {
            ParentWindow::X11(_) => "x11",
            ParentWindow::Win32(_) => "win32",
            ParentWindow::Cocoa(_) => "cocoa",
        }
    }

    /// VST 3 platform type (`IPlugView::attached`).
    pub fn vst3_platform(&self) -> &'static str {
        match self {
            ParentWindow::X11(_) => "X11EmbedWindowID",
            ParentWindow::Win32(_) => "HWND",
            ParentWindow::Cocoa(_) => "NSView",
        }
    }

    /// The handle as a pointer-sized value (an X11 id is widened).
    pub fn raw(&self) -> usize {
        match *self {
            ParentWindow::X11(id) => id as usize,
            ParentWindow::Win32(h) | ParentWindow::Cocoa(h) => h,
        }
    }
}

/// Size of an opened editor, in physical pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditorSize {
    pub width: u32,
    pub height: u32,
    /// The user may resize the host window.
    pub resizable: bool,
}

/// What the plugin asked of the host since the last [`PluginEditor::idle`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EditorEvent {
    /// A parameter was edited in the plugin's interface; `value` is in the
    /// unit of the instance's [`ParamInfo`](crate::ParamInfo).
    Param { id: u32, value: f32 },
    /// The user grabbed a control (begin of an undoable gesture).
    GestureBegin(u32),
    GestureEnd(u32),
    /// The plugin wants its window resized (physical pixels).
    Resize { width: u32, height: u32 },
    /// Resize constraints changed (`resize_hints_changed`, `canResize`).
    HintsChanged,
    Show,
    Hide,
    /// The plugin closed its interface by itself.
    Closed,
}

/// Why a plugin's state is saved or loaded (CLAP `clap.state-context`):
/// some plugins leave things out of a preset that a project keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StateContext {
    /// Inside a project, restored with it.
    Project,
    /// A preset, loaded later into another instance.
    Preset,
    /// A copy of the instance (duplicated device).
    Duplicate,
}

/// A plugin's own interface. All methods run on the UI thread.
pub trait PluginEditor: Send + Sync {
    /// The plugin offers an embeddable interface for this platform.
    fn has_gui(&self) -> bool;

    /// The instance still exists (it may have been replaced or removed).
    fn is_alive(&self) -> bool;

    fn is_open(&self) -> bool;

    /// Creates the interface inside `parent` and shows it; `scale` is the
    /// display scale (1 = 96 dpi).
    fn open(&self, parent: ParentWindow, scale: f64) -> Result<EditorSize, String>;

    /// Hides and destroys the interface (the instance keeps playing).
    fn close(&self);

    /// The user resized the host window: the plugin adjusts the size to its
    /// constraints and takes it. Returns the size it settled on.
    fn set_size(&self, width: u32, height: u32) -> Option<(u32, u32)>;

    /// Resize constraints, for the host window.
    fn resizable(&self) -> bool;

    /// Pumps the plugin's main-thread work (timers, file descriptors, host
    /// callbacks) and collects its requests. Called every frame, even closed:
    /// plugins may run timers without an interface.
    fn idle(&self, out: &mut Vec<EditorEvent>);

    /// The host changed a parameter (knob in Ponzik, undo, automation): the
    /// interface follows. Same unit as [`EditorEvent::Param`].
    fn param_changed(&self, id: u32, value: f32);

    /// The plugin's opaque state (everything a patch is made of, parameters
    /// included), or `None` if it has none or fails to write it. Calls into
    /// the plugin: never on every edit, only when saving.
    fn save_state(&self, _context: StateContext) -> Option<Vec<u8>> {
        None
    }

    /// Restores a state written by [`PluginEditor::save_state`] while the
    /// instance plays, and returns the parameter values read back from the
    /// plugin (same unit as [`EditorEvent::Param`]): the host takes them as
    /// its own values.
    fn load_state(&self, _data: &[u8], _context: StateContext) -> Result<Vec<(u32, f32)>, String> {
        Err(ponzik_i18n::tr("this plugin has no state").into())
    }
}

/// Shared handle on an instance's editor.
pub type Editor = Arc<dyn PluginEditor>;

// --- Run loop --------------------------------------------------------------

/// The descriptor can be read.
pub const FD_READ: u32 = 1 << 0;
/// The descriptor can be written.
pub const FD_WRITE: u32 = 1 << 1;
/// Error or hang-up on the descriptor.
pub const FD_ERROR: u32 = 1 << 2;

#[derive(Clone, Copy, Debug)]
struct Timer {
    id: u32,
    period: Duration,
    next: Instant,
}

/// Timers and watched file descriptors of one plugin instance.
///
/// The loop only keeps the schedule: the owner asks for what is due
/// ([`RunLoop::due_timers`], [`RunLoop::ready_fds`]) and calls the plugin
/// itself, without holding any lock, since a callback may register or remove
/// timers and descriptors.
#[derive(Debug, Default)]
pub struct RunLoop {
    timers: Vec<Timer>,
    fds: Vec<(i32, u32)>,
    next_id: u32,
}

impl RunLoop {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fires every `period_ms` (at least 1 ms, at most every pump); ids start at 1.
    pub fn add_timer(&mut self, period_ms: u32, now: Instant) -> u32 {
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let period = Duration::from_millis(period_ms.max(1) as u64);
        self.timers.push(Timer { id: self.next_id, period, next: now + period });
        self.next_id
    }

    pub fn remove_timer(&mut self, id: u32) -> bool {
        let before = self.timers.len();
        self.timers.retain(|t| t.id != id);
        self.timers.len() != before
    }

    /// Watches `fd` for `flags` ([`FD_READ`]…); false if it is already watched.
    pub fn add_fd(&mut self, fd: i32, flags: u32) -> bool {
        if fd < 0 || self.fds.iter().any(|f| f.0 == fd) {
            return false;
        }
        self.fds.push((fd, flags));
        true
    }

    pub fn modify_fd(&mut self, fd: i32, flags: u32) -> bool {
        match self.fds.iter_mut().find(|f| f.0 == fd) {
            Some(f) => {
                f.1 = flags;
                true
            }
            None => false,
        }
    }

    pub fn remove_fd(&mut self, fd: i32) -> bool {
        let before = self.fds.len();
        self.fds.retain(|f| f.0 != fd);
        self.fds.len() != before
    }

    pub fn is_empty(&self) -> bool {
        self.timers.is_empty() && self.fds.is_empty()
    }

    pub fn timer_count(&self) -> usize {
        self.timers.len()
    }

    pub fn fd_count(&self) -> usize {
        self.fds.len()
    }

    /// Timers due at `now`, each once, rescheduled one period later (a late
    /// pump does not fire a burst of catch-up ticks).
    pub fn due_timers(&mut self, now: Instant) -> Vec<u32> {
        let mut due = Vec::new();
        for t in &mut self.timers {
            if t.next <= now {
                due.push(t.id);
                t.next += t.period;
                if t.next <= now {
                    t.next = now + t.period;
                }
            }
        }
        due
    }

    /// Time until the next timer, for the host's wait.
    pub fn next_deadline(&self, now: Instant) -> Option<Duration> {
        self.timers.iter().map(|t| t.next.saturating_duration_since(now)).min()
    }

    /// Watched descriptors that are ready now, with what happened (no wait).
    pub fn ready_fds(&self) -> Vec<(i32, u32)> {
        poll_fds(&self.fds)
    }
}

#[cfg(unix)]
fn poll_fds(fds: &[(i32, u32)]) -> Vec<(i32, u32)> {
    use std::ffi::c_int;

    #[repr(C)]
    struct PollFd {
        fd: c_int,
        events: i16,
        revents: i16,
    }
    #[cfg(target_os = "linux")]
    type Nfds = std::ffi::c_ulong;
    #[cfg(not(target_os = "linux"))]
    type Nfds = std::ffi::c_uint;
    unsafe extern "C" {
        fn poll(fds: *mut PollFd, n: Nfds, timeout: c_int) -> c_int;
    }
    const POLLIN: i16 = 0x1;
    const POLLOUT: i16 = 0x4;
    const POLLERR: i16 = 0x8;
    const POLLHUP: i16 = 0x10;
    const POLLNVAL: i16 = 0x20;

    if fds.is_empty() {
        return Vec::new();
    }
    let mut set: Vec<PollFd> = fds
        .iter()
        .map(|&(fd, flags)| {
            let mut events = 0;
            if flags & FD_READ != 0 {
                events |= POLLIN;
            }
            if flags & FD_WRITE != 0 {
                events |= POLLOUT;
            }
            PollFd { fd, events, revents: 0 }
        })
        .collect();
    // SAFETY: `set` holds `set.len()` initialized entries; no wait.
    if unsafe { poll(set.as_mut_ptr(), set.len() as Nfds, 0) } <= 0 {
        return Vec::new();
    }
    set.iter()
        .filter(|p| p.revents & POLLNVAL == 0)
        .filter_map(|p| {
            let mut flags = 0;
            if p.revents & POLLIN != 0 {
                flags |= FD_READ;
            }
            if p.revents & POLLOUT != 0 {
                flags |= FD_WRITE;
            }
            if p.revents & (POLLERR | POLLHUP) != 0 {
                flags |= FD_ERROR;
            }
            (flags != 0).then_some((p.fd, flags))
        })
        .collect()
}

#[cfg(not(unix))]
fn poll_fds(_: &[(i32, u32)]) -> Vec<(i32, u32)> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timers_fire_once_per_period() {
        let t0 = Instant::now();
        let mut rl = RunLoop::new();
        let a = rl.add_timer(10, t0);
        let b = rl.add_timer(25, t0);
        assert_ne!(a, b);
        assert!(rl.due_timers(t0).is_empty());
        assert_eq!(rl.next_deadline(t0), Some(Duration::from_millis(10)));
        assert_eq!(rl.due_timers(t0 + Duration::from_millis(10)), [a]);
        // Very late pump: one tick each, no burst.
        assert_eq!(rl.due_timers(t0 + Duration::from_millis(100)), [a, b]);
        assert!(rl.due_timers(t0 + Duration::from_millis(105)).is_empty());
        assert_eq!(rl.due_timers(t0 + Duration::from_millis(110)), [a]);
        assert!(rl.remove_timer(a));
        assert!(!rl.remove_timer(a));
        assert_eq!(rl.timer_count(), 1);
        // A zero period is clamped to 1 ms.
        let c = rl.add_timer(0, t0);
        assert!(rl.due_timers(t0 + Duration::from_millis(1)).contains(&c));
    }

    #[test]
    fn fds_are_unique_and_editable() {
        let mut rl = RunLoop::new();
        assert!(rl.add_fd(5, FD_READ));
        assert!(!rl.add_fd(5, FD_WRITE));
        assert!(!rl.add_fd(-1, FD_READ));
        assert!(rl.modify_fd(5, FD_READ | FD_WRITE));
        assert!(!rl.modify_fd(6, FD_READ));
        assert!(rl.remove_fd(5));
        assert!(rl.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn readable_pipe_is_reported() {
        use std::io::Write;
        use std::os::fd::AsRawFd;
        use std::os::unix::net::UnixStream;

        let (mut a, b) = UnixStream::pair().expect("socket pair");
        let mut rl = RunLoop::new();
        rl.add_fd(b.as_raw_fd(), FD_READ);
        assert!(rl.ready_fds().is_empty(), "nothing written yet");
        a.write_all(b"x").unwrap();
        assert_eq!(rl.ready_fds(), [(b.as_raw_fd(), FD_READ)]);
        drop(a);
        // Peer gone: still readable (end of file), with the hang-up.
        let ready = rl.ready_fds();
        assert_eq!(ready.len(), 1);
        assert!(ready[0].1 & FD_READ != 0);
    }
}
