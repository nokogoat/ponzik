//! Background renders of the warped audio clips.
//!
//! After each edit the interface lists the render every warped clip needs;
//! the missing ones go to a worker thread, the engine plays those clips
//! resampled meanwhile (in time, the pitch following the tempo), and the
//! sequence is sent again as soon as a render lands in the cache. Stale jobs
//! (a tempo dragged through many values) are skipped, unused renders pruned.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use ponzik_engine::Command;
use ponzik_project::warp::{self, WarpJob};

use super::app::App;

/// Renders kept beyond those of the current project (for undo).
const SPARE_RENDERS: usize = 12;

pub struct Warping {
    jobs: Option<Sender<WarpJob>>,
    done_tx: Sender<u64>,
    done: Receiver<u64>,
    /// Renders the project needs now; the worker skips the others.
    wanted: Arc<Mutex<Vec<u64>>>,
    pending: Vec<u64>,
    /// Render needed by each audio clip (`None`: none, or plays as is).
    pub keys: Vec<Option<u64>>,
    /// Renders needed by the audio clips of the session's slots.
    session_keys: Vec<u64>,
}

impl Default for Warping {
    fn default() -> Self {
        let (done_tx, done) = channel();
        Self {
            jobs: None,
            done_tx,
            done,
            wanted: Arc::default(),
            pending: Vec::new(),
            keys: Vec::new(),
            session_keys: Vec::new(),
        }
    }
}

impl Warping {
    /// True while clip `i`'s stretch is being rendered.
    pub fn is_pending(&self, i: usize) -> bool {
        self.keys
            .get(i)
            .copied()
            .flatten()
            .is_some_and(|k| self.pending.contains(&k))
    }

    fn submit(&mut self, job: WarpJob) {
        if self.jobs.is_none() {
            let (tx, rx) = channel::<WarpJob>();
            let (wanted, done) = (self.wanted.clone(), self.done_tx.clone());
            let spawned = std::thread::Builder::new()
                .name("ponzik-warp".into())
                .spawn(move || {
                    while let Ok(job) = rx.recv() {
                        let wanted = wanted.lock().map(|w| w.contains(&job.key)).unwrap_or(true);
                        if wanted {
                            warp::render_cached(&job);
                        }
                        if done.send(job.key).is_err() {
                            break;
                        }
                    }
                });
            if spawned.is_err() {
                // No thread: render here, the interface waits once.
                warp::render_cached(&job);
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
}

impl App {
    /// Lists the renders the warped clips need and queues the missing ones.
    pub(super) fn refresh_warps(&mut self) {
        let sr = self.sample_rate as f32;
        let map = self.project.tempo_map();
        let mut jobs = Vec::new();
        let keys: Vec<Option<u64>> = self
            .project
            .audio_clips
            .iter()
            .map(|c| {
                let sample = self.samples.get(&c.path)?;
                let job = WarpJob::for_clip(c, sample, &map, sr)?;
                let key = job.key;
                jobs.push(job);
                Some(key)
            })
            .collect();
        let session = {
            let loader = |p: &str| self.samples.get(p).cloned();
            self.project.session_warp_jobs(sr, &loader)
        };
        let session_keys: Vec<u64> = session.iter().map(|j| j.key).collect();
        jobs.extend(session);
        let w = &mut self.warping;
        let mut wanted: Vec<u64> = keys.iter().flatten().copied().collect();
        wanted.extend(&session_keys);
        w.session_keys = session_keys;
        if let Ok(mut list) = w.wanted.lock() {
            list.clone_from(&wanted);
        }
        w.keys = keys;
        for job in jobs {
            if !w.pending.contains(&job.key) && warp::cached(job.key).is_none() {
                w.submit(job);
            }
        }
        warp::prune(&wanted, SPARE_RENDERS);
    }

    /// Finished renders: the engine gets the sequence again with them.
    pub(super) fn poll_warps(&mut self) {
        let mut landed = false;
        while let Ok(key) = self.warping.done.try_recv() {
            self.warping.pending.retain(|&k| k != key);
            landed |= self.warping.keys.contains(&Some(key)) || self.warping.session_keys.contains(&key);
        }
        if landed {
            let seq = {
                let loader = |p: &str| self.samples.get(p).cloned();
                self.synced
                    .sequence_for(self.sample_rate as f32, &loader, &|job| {
                        warp::cached(job.key)
                    })
            };
            self.send(Command::SetSequence(Box::new(seq)));
        }
    }
}
