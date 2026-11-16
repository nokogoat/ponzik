//! The bundled sound library: built in the background at first run (or after
//! an update that changed its sounds), then listed first in the browser.

use std::sync::mpsc::{self, TryRecvError};

use ponzik_i18n::{tr, tr_fmt};
use ponzik_project::library;

use super::app::App;
use super::compat;

impl App {
    /// Starts building the library if it is missing or out of date.
    /// `PONZIK_NO_LIBRARY=1` skips it.
    pub fn start_library(&mut self) {
        let dir = compat::library_dir();
        if library::library_is_current(&dir) || std::env::var_os("PONZIK_NO_LIBRARY").is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        // Half the cores: the interface and the audio keep theirs.
        let threads = std::thread::available_parallelism().map_or(2, |n| n.get() / 2).max(1);
        let spawned = std::thread::Builder::new().name("ponzik-library".into()).spawn(move || {
            let _ = tx.send(library::generate_library(&dir, threads, &|_, _| {}));
        });
        if spawned.is_ok() {
            self.library_build = Some(rx);
            self.toast(tr("Building the sound library…"));
        }
    }

    /// Picks up the finished library; true when the interface must redraw.
    pub fn library_tick(&mut self) -> bool {
        let Some(rx) = &self.library_build else { return false };
        let result = match rx.try_recv() {
            Ok(r) => r,
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => Err("interrupted".into()),
        };
        self.library_build = None;
        match result {
            Ok(n) => {
                self.sample_files = compat::scan_samples();
                self.toast(&tr_fmt!("Sound library ready: {} sounds in the browser", n));
            }
            Err(e) => self.toast(&tr_fmt!("Sound library: {}", e)),
        }
        true
    }
}
