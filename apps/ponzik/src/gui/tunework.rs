//! Background work of the Pitch Editor: analysing a take when the editor
//! opens on it, and rendering the pitch edits of the project.
//!
//! After each edit the interface lists the renders the edits need; the
//! missing ones go to a worker thread (stale ones, from a drag through many
//! positions, are skipped), the playlist plays the previous render or the
//! file meanwhile, and the sequence is sent again as soon as a render lands.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use ponzik_engine::Command;
use ponzik_engine::pitchedit::Analysis;
use ponzik_project::tune::{self, TuneJob};

use super::app::App;

/// Renders kept beyond those of the current project (for undo).
const SPARE_RENDERS: usize = 8;

/// An analysis running in its own thread.
struct Analysing {
    path: String,
    progress: Arc<AtomicU32>,
    cancel: Arc<AtomicBool>,
    done: Receiver<Option<Arc<Analysis>>>,
}

pub struct Tuning {
    jobs: Option<Sender<TuneJob>>,
    done_tx: Sender<u64>,
    done: Receiver<u64>,
    /// Renders the project needs now; the worker skips the others.
    wanted: Arc<Mutex<Vec<u64>>>,
    pending: Vec<u64>,
    /// Progress of the render under way (‰).
    progress: Arc<AtomicU32>,
    analysing: Option<Analysing>,
}

impl Default for Tuning {
    fn default() -> Self {
        let (done_tx, done) = channel();
        Self {
            jobs: None,
            done_tx,
            done,
            wanted: Arc::default(),
            pending: Vec::new(),
            progress: Arc::default(),
            analysing: None,
        }
    }
}

impl Tuning {
    /// Renders not done yet, and the progress of the one under way (0 … 1).
    pub fn rendering(&self) -> Option<(usize, f32)> {
        (!self.pending.is_empty()).then(|| (self.pending.len(), self.progress.load(Ordering::Relaxed) as f32 / 1000.0))
    }

    /// The file being analysed and how far along (0 … 1).
    pub fn analysing(&self) -> Option<(&str, f32)> {
        self.analysing.as_ref().map(|a| (a.path.as_str(), a.progress.load(Ordering::Relaxed) as f32 / 1000.0))
    }

    fn submit(&mut self, job: TuneJob) {
        if self.jobs.is_none() {
            let (tx, rx) = channel::<TuneJob>();
            let (wanted, done, progress) = (self.wanted.clone(), self.done_tx.clone(), self.progress.clone());
            let spawned = std::thread::Builder::new().name("ponzik-pitch".into()).spawn(move || {
                while let Ok(job) = rx.recv() {
                    let wanted = wanted.lock().map(|w| w.contains(&job.key)).unwrap_or(true);
                    if wanted {
                        progress.store(0, Ordering::Relaxed);
                        job.run(Some(&progress), None);
                    }
                    if done.send(job.key).is_err() {
                        break;
                    }
                }
            });
            if spawned.is_err() {
                // No thread: render here, the interface waits once.
                job.run(None, None);
                return;
            }
            self.jobs = Some(tx);
        }
        let key = job.key;
        if let Some(tx) = &self.jobs
            && tx.send(job).is_ok()
        {
            self.pending.push(key);
        }
    }

    /// Stops the analysis under way.
    pub fn cancel_analysis(&mut self) {
        if let Some(a) = self.analysing.take() {
            a.cancel.store(true, Ordering::Relaxed);
        }
    }
}

impl App {
    /// Lists the renders the pitch edits need and queues the missing ones.
    pub(super) fn refresh_tunes(&mut self) {
        let jobs = {
            let loader = |p: &str| self.samples.get(p).cloned();
            self.project.tune_jobs(&loader)
        };
        let wanted: Vec<u64> = jobs.iter().map(|j| j.key).collect();
        if let Ok(mut list) = self.tuning.wanted.lock() {
            list.clone_from(&wanted);
        }
        for job in jobs {
            if !self.tuning.pending.contains(&job.key) && tune::cached(job.key).is_none() {
                self.tuning.submit(job);
            }
        }
        tune::prune(&wanted, SPARE_RENDERS);
    }

    /// Sends the sequence again (a render landed, A/B switched).
    pub(super) fn resend_sequence(&mut self) {
        // Warped clips of an edited file stretch its render: their keys changed.
        self.refresh_warps();
        let seq = {
            let loader = |p: &str| self.samples.get(p).cloned();
            self.synced.sequence_for(self.sample_rate as f32, &loader, &|job| ponzik_project::warp::cached(job.key))
        };
        self.send(Command::SetSequence(Box::new(seq)));
    }

    /// Finished renders and analyses.
    pub(super) fn poll_tunes(&mut self) {
        let mut landed = false;
        while let Ok(key) = self.tuning.done.try_recv() {
            self.tuning.pending.retain(|&k| k != key);
            landed |= tune::cached(key).is_some();
        }
        if landed {
            self.resend_sequence();
        }
        if let Some(a) = &self.tuning.analysing
            && let Ok(result) = a.done.try_recv()
        {
            let path = a.path.clone();
            self.tuning.analysing = None;
            match result {
                Some(analysis) => self.pitch_analysed(&path, analysis),
                None => self.toast(ponzik_i18n::tr("Could not analyse the recording")),
            }
        }
    }

    /// Analyses a file in the background (at once when it is cached).
    pub(super) fn request_analysis(&mut self, path: &str) {
        let Some(sample) = self.samples.get(path).cloned() else { return };
        if self.tuning.analysing.as_ref().is_some_and(|a| a.path == path) {
            return;
        }
        self.tuning.cancel_analysis();
        if let Some(a) = tune::analysis(&sample) {
            self.pitch_analysed(path, a);
            return;
        }
        let (tx, done) = channel();
        let progress = Arc::new(AtomicU32::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let (p, k) = (progress.clone(), cancel.clone());
        let spawned = std::thread::Builder::new().name("ponzik-pitch-analysis".into()).spawn(move || {
            let _ = tx.send(tune::analyze(&sample, Some(&p), Some(&k)));
        });
        match spawned {
            Ok(_) => self.tuning.analysing = Some(Analysing { path: path.to_string(), progress, cancel, done }),
            Err(_) => {
                if let Some(a) = self.samples.get(path).cloned().and_then(|s| tune::analyze(&s, None, None)) {
                    self.pitch_analysed(path, a);
                }
            }
        }
    }
}
