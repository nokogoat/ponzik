//! Worker pool of the audio graph: independent work of a block (channels,
//! mixer tracks whose inputs are ready) runs on a few threads created when the
//! engine starts, the audio thread taking part in the work.
//!
//! Real-time rules: no lock and no allocation per block. A job is a set of
//! tasks with an optional dependency graph ([`Graph`]); each task has a
//! counter of unfinished prerequisites, and the last prerequisite to finish
//! pushes it in a lock-free queue. Idle workers spin a little, then park
//! ([`std::thread::park`], a futex on Linux) until the next job.
//!
//! The caller never waits for a worker that has not started: the audio thread
//! also takes tasks, so a pool whose workers are asleep or descheduled still
//! finishes the job alone. It only waits for tasks already started elsewhere.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU16, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Most tasks in one job (channels of the rack, tracks of the mixer).
pub(crate) const MAX_TASKS: usize = 128;
/// Most dependencies in one graph.
pub(crate) const MAX_EDGES: usize = 512;
/// Most worker threads, whatever the machine.
pub const MAX_WORKERS: usize = 15;
/// Most worker threads chosen by default. The graph of a song is rarely wider
/// than this, and on SMT machines more spinning workers end up on the sibling
/// of a busy core and slow it down.
pub const DEFAULT_MAX_WORKERS: usize = 4;
/// How long an idle worker spins before it parks: long enough to catch the
/// next job of the same audio block, short enough not to burn a core between
/// blocks.
const SPIN: Duration = Duration::from_micros(20);
/// Free slot of the ready queue.
const EMPTY: u16 = u16::MAX;

/// Worker threads to use by default: one less than the cores (the audio
/// thread works too), at most [`DEFAULT_MAX_WORKERS`]. The `PONZIK_AUDIO_THREADS`
/// environment variable overrides it, up to [`MAX_WORKERS`] (0: single-threaded engine).
pub fn default_worker_threads() -> usize {
    if let Some(n) = std::env::var("PONZIK_AUDIO_THREADS").ok().and_then(|v| v.trim().parse::<usize>().ok()) {
        return n.min(MAX_WORKERS);
    }
    thread::available_parallelism().map_or(0, |n| n.get().saturating_sub(1)).min(DEFAULT_MAX_WORKERS)
}

/// Dependencies between the tasks of a job, built on the audio thread without
/// allocation: `edge(a, b)` means `b` starts after `a` is done.
pub(crate) struct Graph {
    count: usize,
    deps: [u16; MAX_TASKS],
    /// Dependents of task `t`: `next[start[t]..start[t + 1]]`.
    start: [u16; MAX_TASKS + 1],
    next: [u16; MAX_EDGES],
    raw: [(u16, u16); MAX_EDGES],
    edges: usize,
}

impl Graph {
    pub(crate) fn new() -> Self {
        Self {
            count: 0,
            deps: [0; MAX_TASKS],
            start: [0; MAX_TASKS + 1],
            next: [0; MAX_EDGES],
            raw: [(0, 0); MAX_EDGES],
            edges: 0,
        }
    }

    pub(crate) fn count(&self) -> usize {
        self.count
    }

    pub(crate) fn reset(&mut self, count: usize) {
        debug_assert!(count <= MAX_TASKS);
        self.count = count.min(MAX_TASKS);
        self.edges = 0;
    }

    /// `to` runs after `from`; false if the graph is full.
    pub(crate) fn edge(&mut self, from: usize, to: usize) -> bool {
        debug_assert!(from < self.count && to < self.count && from != to);
        if self.edges == MAX_EDGES {
            return false;
        }
        self.raw[self.edges] = (from as u16, to as u16);
        self.edges += 1;
        true
    }

    /// Builds the dependent lists once every edge is in.
    pub(crate) fn finish(&mut self) {
        let n = self.count;
        self.deps[..n].fill(0);
        self.start[..=n].fill(0);
        for &(from, to) in &self.raw[..self.edges] {
            self.deps[to as usize] += 1;
            self.start[from as usize + 1] += 1;
        }
        for t in 0..n {
            self.start[t + 1] += self.start[t];
        }
        // `start[t]` serves as the write cursor, then is shifted back.
        for &(from, to) in &self.raw[..self.edges] {
            let at = &mut self.start[from as usize];
            self.next[*at as usize] = to;
            *at += 1;
        }
        for t in (1..=n).rev() {
            self.start[t] = self.start[t - 1];
        }
        self.start[0] = 0;
    }

    fn dependents(&self, task: usize) -> &[u16] {
        &self.next[self.start[task] as usize..self.start[task + 1] as usize]
    }
}

/// A job published to the workers; lives on the caller's stack for the
/// duration of [`WorkerPool::run`].
struct Job<'a> {
    count: usize,
    graph: Option<&'a Graph>,
    task: &'a (dyn Fn(usize) + Sync),
}

struct Shared {
    /// Current job (`*const Job`), null between jobs.
    job: AtomicPtr<()>,
    /// Bumped at every job: wakes the spinning workers.
    epoch: AtomicU32,
    /// Workers inside the current job.
    busy: AtomicUsize,
    quit: AtomicBool,
    /// Worker `i` is parked (or about to be) and needs an unpark.
    sleeping: Box<[AtomicBool]>,
    /// Unfinished prerequisites of each task.
    pending: Box<[AtomicU16]>,
    /// Ready tasks, in push order; `head` is the next one to take.
    queue: Box<[AtomicU16]>,
    head: Padded<AtomicUsize>,
    tail: Padded<AtomicUsize>,
    /// Tasks not finished yet.
    remaining: Padded<AtomicUsize>,
    /// Time spent inside tasks, all threads together, and wall time of the
    /// jobs on the calling thread, in nanoseconds (see [`WorkerPool::take_times`]).
    work_ns: Padded<AtomicU64>,
    job_ns: AtomicU64,
}

/// A counter alone on its cache line: threads that write it do not slow down
/// the ones reading its neighbours.
#[repr(align(64))]
struct Padded<T>(T);

impl<T> std::ops::Deref for Padded<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.0
    }
}

impl Shared {
    /// Takes and runs tasks until the job is done.
    fn work(&self, job: &Job) {
        let mut idle = 0u32;
        loop {
            let h = self.head.load(Ordering::Acquire);
            if h < self.tail.load(Ordering::Acquire) {
                if self.head.compare_exchange_weak(h, h + 1, Ordering::AcqRel, Ordering::Relaxed).is_err() {
                    continue;
                }
                idle = 0;
                let task = match job.graph {
                    None => h,
                    Some(_) => loop {
                        // The pusher reserved the slot before writing it.
                        let t = self.queue[h].load(Ordering::Acquire);
                        if t != EMPTY {
                            break t as usize;
                        }
                        std::hint::spin_loop();
                    },
                };
                let started = Instant::now();
                (job.task)(task);
                self.work_ns.fetch_add(started.elapsed().as_nanos() as u64, Ordering::Relaxed);
                if let Some(graph) = job.graph {
                    for &d in graph.dependents(task) {
                        if self.pending[d as usize].fetch_sub(1, Ordering::AcqRel) == 1 {
                            let slot = self.tail.fetch_add(1, Ordering::AcqRel);
                            self.queue[slot].store(d, Ordering::Release);
                        }
                    }
                }
                self.remaining.fetch_sub(1, Ordering::AcqRel);
            } else if self.remaining.load(Ordering::Acquire) == 0 {
                return;
            } else {
                // A task runs elsewhere and holds back the next ones.
                idle += 1;
                if idle < 1 << 14 {
                    std::hint::spin_loop();
                } else {
                    thread::yield_now();
                }
            }
        }
    }
}

/// Worker threads of the engine; dropped (joined) with it.
pub struct WorkerPool {
    shared: Arc<Shared>,
    threads: Vec<JoinHandle<()>>,
}

impl WorkerPool {
    /// Starts `workers` threads (raised to real-time priority when allowed).
    pub fn new(workers: usize) -> Self {
        let workers = workers.min(MAX_WORKERS);
        let shared = Arc::new(Shared {
            job: AtomicPtr::new(std::ptr::null_mut()),
            epoch: AtomicU32::new(0),
            busy: AtomicUsize::new(0),
            quit: AtomicBool::new(false),
            sleeping: (0..workers).map(|_| AtomicBool::new(false)).collect(),
            pending: (0..MAX_TASKS).map(|_| AtomicU16::new(0)).collect(),
            queue: (0..MAX_TASKS).map(|_| AtomicU16::new(EMPTY)).collect(),
            head: Padded(AtomicUsize::new(0)),
            tail: Padded(AtomicUsize::new(0)),
            remaining: Padded(AtomicUsize::new(0)),
            work_ns: Padded(AtomicU64::new(0)),
            job_ns: AtomicU64::new(0),
        });
        let threads = (0..workers)
            .filter_map(|i| {
                let shared = shared.clone();
                thread::Builder::new().name(format!("ponzik-dsp-{i}")).spawn(move || worker(&shared, i)).ok()
            })
            .collect();
        Self { shared, threads }
    }

    pub fn workers(&self) -> usize {
        self.threads.len()
    }

    /// Time spent in the tasks of the jobs run since the last call (all threads
    /// together) and wall time of those jobs, in nanoseconds. Their difference
    /// is what the jobs saved compared to running alone on the calling thread.
    pub(crate) fn take_times(&self) -> (u64, u64) {
        let s = &*self.shared;
        (s.work_ns.swap(0, Ordering::Relaxed), s.job_ns.swap(0, Ordering::Relaxed))
    }

    /// Runs `task(0..count)` in any order, on the workers and the calling thread.
    pub(crate) fn run(&self, count: usize, task: &(dyn Fn(usize) + Sync)) {
        self.execute(&Job { count: count.min(MAX_TASKS), graph: None, task });
    }

    /// Runs the tasks of `graph`, each one after its prerequisites.
    pub(crate) fn run_graph(&self, graph: &Graph, task: &(dyn Fn(usize) + Sync)) {
        self.execute(&Job { count: graph.count, graph: Some(graph), task });
    }

    fn execute(&self, job: &Job) {
        if job.count == 0 {
            return;
        }
        let s = &*self.shared;
        let started = Instant::now();
        // No worker is inside a job here (see the end of this function).
        s.head.store(0, Ordering::Relaxed);
        s.remaining.store(job.count, Ordering::Relaxed);
        match job.graph {
            None => s.tail.store(job.count, Ordering::Relaxed),
            Some(graph) => {
                let mut tail = 0;
                for t in 0..job.count {
                    s.queue[t].store(EMPTY, Ordering::Relaxed);
                    s.pending[t].store(graph.deps[t], Ordering::Relaxed);
                }
                for t in (0..job.count).filter(|&t| graph.deps[t] == 0) {
                    s.queue[tail].store(t as u16, Ordering::Relaxed);
                    tail += 1;
                }
                s.tail.store(tail, Ordering::Relaxed);
            }
        }
        s.job.store(job as *const Job as *mut (), Ordering::SeqCst);
        s.epoch.fetch_add(1, Ordering::SeqCst);
        // Wake no more workers than there are tasks to share.
        for (i, t) in self.threads.iter().enumerate().take(job.count - 1) {
            if s.sleeping[i].swap(false, Ordering::SeqCst) {
                t.thread().unpark();
            }
        }
        s.work(job);
        // Close the job, then wait for the workers still inside it: a worker
        // that enters later sees the null job and leaves without touching it.
        s.job.store(std::ptr::null_mut(), Ordering::SeqCst);
        while s.busy.load(Ordering::SeqCst) != 0 {
            std::hint::spin_loop();
        }
        s.job_ns.fetch_add(started.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }
}

/// Load above which the workers are woken up: the share of the block budget
/// that rendering on the audio thread alone would take (smoothed).
const PARALLEL_ON: f32 = 0.40;
/// A single block this heavy wakes the workers at once.
const PARALLEL_SPIKE: f32 = 0.70;
/// Load under which the workers may go back to sleep…
const PARALLEL_OFF: f32 = 0.20;
/// …once it stayed there this long, in seconds.
const PARALLEL_CALM: f32 = 3.0;
/// Time constant of the smoothed load, in seconds.
const PARALLEL_TAU: f32 = 0.2;

/// Decides, block after block, whether the worker threads are worth waking
/// up. A light project renders on the audio thread alone and the workers stay
/// parked (no CPU at all); once the load nears the budget, the work is shared
/// again. Hysteresis keeps it from flapping: on fast, off slowly.
///
/// The output does not depend on the choice: it is bit-identical either way.
#[derive(Clone, Debug, Default)]
pub struct Parallelism {
    on: bool,
    smoothed: f32,
    calm: f32,
}

impl Parallelism {
    /// Feeds the serial-equivalent load of one block (time the audio thread
    /// alone would have spent / duration of the block) lasting `secs` seconds;
    /// returns whether the next blocks should use the workers.
    pub fn update(&mut self, load: f32, secs: f32) -> bool {
        let load = if load.is_finite() { load.max(0.0) } else { 1.0 };
        let a = (secs / PARALLEL_TAU).clamp(0.0, 1.0);
        self.smoothed += (load - self.smoothed) * a;
        if !self.on {
            if self.smoothed > PARALLEL_ON || load > PARALLEL_SPIKE {
                self.on = true;
                self.calm = 0.0;
            }
        } else if self.smoothed < PARALLEL_OFF {
            self.calm += secs;
            if self.calm >= PARALLEL_CALM {
                self.on = false;
            }
        } else {
            self.calm = 0.0;
        }
        self.on
    }

    /// Whether the workers are in use.
    pub fn active(&self) -> bool {
        self.on
    }
}

impl Drop for WorkerPool {
    fn drop(&mut self) {
        self.shared.quit.store(true, Ordering::SeqCst);
        for t in &self.threads {
            t.thread().unpark();
        }
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

fn worker(s: &Shared, idx: usize) {
    promote_current_thread();
    // Same arithmetic as the audio thread (see `Engine::process`).
    let _flush = crate::FlushDenormals::new();
    let mut seen = s.epoch.load(Ordering::Acquire);
    loop {
        // Wait for the next job: spin, then park.
        let since = Instant::now();
        let mut spins = 0u32;
        loop {
            if s.quit.load(Ordering::Acquire) {
                return;
            }
            let e = s.epoch.load(Ordering::Acquire);
            if e != seen {
                seen = e;
                break;
            }
            spins += 1;
            if spins % 64 != 0 || since.elapsed() < SPIN {
                std::hint::spin_loop();
                continue;
            }
            s.sleeping[idx].store(true, Ordering::SeqCst);
            if s.epoch.load(Ordering::SeqCst) == seen && !s.quit.load(Ordering::SeqCst) {
                thread::park();
            }
            s.sleeping[idx].store(false, Ordering::SeqCst);
        }
        s.busy.fetch_add(1, Ordering::SeqCst);
        let job = s.job.load(Ordering::SeqCst) as *const Job;
        if !job.is_null() {
            // SAFETY: the publisher keeps the job alive until `busy` drops back
            // to zero after it nulled the pointer; we counted ourselves in
            // `busy` before reading it.
            s.work(unsafe { &*job });
        }
        s.busy.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Raises the calling thread to real-time priority if the system allows it:
/// `SCHED_FIFO` on Linux (needs `RLIMIT_RTPRIO` or `CAP_SYS_NICE`), MMCSS
/// "Pro Audio" on Windows. Returns false when it stays at normal priority.
pub fn promote_current_thread() -> bool {
    os::promote()
}

#[cfg(target_os = "linux")]
mod os {
    #[repr(C)]
    struct SchedParam {
        sched_priority: i32,
    }

    unsafe extern "C" {
        fn pthread_self() -> usize;
        fn pthread_setschedparam(thread: usize, policy: i32, param: *const SchedParam) -> i32;
        fn sched_get_priority_min(policy: i32) -> i32;
        fn sched_get_priority_max(policy: i32) -> i32;
    }

    const SCHED_FIFO: i32 = 1;
    /// Below the usual priority of audio server threads (JACK, PipeWire: 80+),
    /// above everything that is not audio.
    const PRIORITY: i32 = 70;

    pub(super) fn promote() -> bool {
        // SAFETY: plain libc calls on the current thread; failure (EPERM) leaves
        // the scheduling unchanged.
        unsafe {
            let (lo, hi) = (sched_get_priority_min(SCHED_FIFO), sched_get_priority_max(SCHED_FIFO));
            if lo < 0 || hi < lo {
                return false;
            }
            let param = SchedParam { sched_priority: PRIORITY.clamp(lo, hi) };
            pthread_setschedparam(pthread_self(), SCHED_FIFO, &param) == 0
        }
    }
}

#[cfg(windows)]
mod os {
    use std::ffi::c_void;

    #[link(name = "avrt")]
    unsafe extern "system" {
        fn AvSetMmThreadCharacteristicsW(task: *const u16, index: *mut u32) -> *mut c_void;
    }

    pub(super) fn promote() -> bool {
        let task: Vec<u16> = "Pro Audio\0".encode_utf16().collect();
        let mut index = 0;
        // SAFETY: NUL-terminated UTF-16 string. The handle is never reverted:
        // the worker keeps the priority until it exits.
        !unsafe { AvSetMmThreadCharacteristicsW(task.as_ptr(), &mut index) }.is_null()
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
mod os {
    pub(super) fn promote() -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    #[test]
    fn every_task_runs_once() {
        let pool = WorkerPool::new(3);
        let hits: Vec<AtomicU32> = (0..MAX_TASKS).map(|_| AtomicU32::new(0)).collect();
        for count in [0, 1, 2, 7, MAX_TASKS] {
            for h in &hits {
                h.store(0, Ordering::Relaxed);
            }
            pool.run(count, &|t| {
                hits[t].fetch_add(1, Ordering::Relaxed);
            });
            assert!(hits[..count].iter().all(|h| h.load(Ordering::Relaxed) == 1));
            assert!(hits[count..].iter().all(|h| h.load(Ordering::Relaxed) == 0));
        }
    }

    #[test]
    fn graph_runs_tasks_after_their_prerequisites() {
        let pool = WorkerPool::new(4);
        let mut graph = Graph::new();
        // Diamond chains: 0 → {1, 2} → 3 → 4, plus 5 alone and 6 after 2 and 5.
        graph.reset(7);
        for (a, b) in [(0, 1), (0, 2), (1, 3), (2, 3), (3, 4), (2, 6), (5, 6)] {
            assert!(graph.edge(a, b));
        }
        graph.finish();
        let clock = AtomicU64::new(1);
        let done: Vec<AtomicU64> = (0..7).map(|_| AtomicU64::new(0)).collect();
        let started: Vec<AtomicU64> = (0..7).map(|_| AtomicU64::new(0)).collect();
        for _ in 0..2_000 {
            pool.run_graph(&graph, &|t| {
                started[t].store(clock.fetch_add(1, Ordering::SeqCst), Ordering::SeqCst);
                done[t].store(clock.fetch_add(1, Ordering::SeqCst), Ordering::SeqCst);
            });
            let (s, d) = (|t: usize| started[t].load(Ordering::SeqCst), |t: usize| done[t].load(Ordering::SeqCst));
            for (a, b) in [(0, 1), (0, 2), (1, 3), (2, 3), (3, 4), (2, 6), (5, 6)] {
                assert!(d(a) < s(b), "{a} before {b}");
            }
        }
    }

    #[test]
    fn a_pool_without_workers_runs_on_the_caller() {
        let pool = WorkerPool::new(0);
        let sum = AtomicUsize::new(0);
        pool.run(10, &|t| {
            sum.fetch_add(t, Ordering::Relaxed);
        });
        assert_eq!(sum.load(Ordering::Relaxed), 45);
    }

    #[test]
    fn parallelism_switches_on_fast_and_off_slowly() {
        let block = 256.0 / 48_000.0;
        let mut p = Parallelism::default();
        // Light load: the workers stay asleep.
        for _ in 0..1_000 {
            assert!(!p.update(0.15, block));
        }
        // A sustained heavy load wakes them within a fraction of a second.
        let mut blocks = 0;
        while !p.update(0.55, block) {
            blocks += 1;
            assert!(blocks < 60, "too slow to switch on");
        }
        // Spread on the workers, the load drops: they stay on for a while…
        let mut calm = 0.0;
        while p.update(0.12, block) {
            calm += block;
            assert!(calm < 10.0, "never switches off");
        }
        assert!(calm >= PARALLEL_CALM, "switched off after {calm} s");
        // …and a load between the two thresholds changes nothing.
        for _ in 0..2_000 {
            assert!(!p.update(0.30, block));
        }
        // A single block close to the budget wakes them at once.
        assert!(p.update(0.9, block));
        // Back between the thresholds: still on, the calm period restarts.
        for _ in 0..2_000 {
            assert!(p.update(0.30, block));
        }
        // A load that cannot be measured counts as a full one.
        assert!(Parallelism::default().update(f32::NAN, block));
    }

    #[test]
    fn job_times_are_measured() {
        let pool = WorkerPool::new(2);
        pool.run(8, &|_| thread::sleep(Duration::from_micros(200)));
        let (work, wall) = pool.take_times();
        assert!(work >= 8 * 200_000, "{work}");
        // Wall time: at least one task, at most all of them plus the overhead.
        assert!(wall >= 200_000 && wall <= work + 1_000_000, "{wall} / {work}");
        assert_eq!(pool.take_times(), (0, 0));
    }

    #[test]
    fn workers_wake_up_after_parking() {
        let pool = WorkerPool::new(2);
        for _ in 0..5 {
            thread::sleep(Duration::from_millis(2));
            let sum = AtomicUsize::new(0);
            pool.run(MAX_TASKS, &|t| {
                sum.fetch_add(t, Ordering::Relaxed);
            });
            assert_eq!(sum.load(Ordering::Relaxed), MAX_TASKS * (MAX_TASKS - 1) / 2);
        }
    }
}
