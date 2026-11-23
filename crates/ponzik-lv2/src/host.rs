//! Host features given to an instance: `log:log`, `options:options`
//! (sample rate, block lengths, sequence size), `worker:schedule` with its
//! worker thread, and the URIDs Ponzik uses.
//!
//! Also the state an instance shares with its editor on the UI thread
//! ([`Shared`]): the instance pointer, a gate that keeps `run` and the UI
//! thread's instantiation-class calls (state restore) apart without the
//! audio thread ever waiting, the control values the audio thread
//! publishes, and the lock-free queues between the plugin and its interface.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU8, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::thread::{JoinHandle, Thread};
use std::time::Duration;

use ponzik_engine::EditorEvent;
use ponzik_engine::ring::{Consumer, Producer};

use crate::abi::*;
use crate::bytes::ByteRing;
use crate::urid::map;

/// URIDs looked up once.
pub(crate) struct Urids {
    pub sequence: u32,
    pub chunk: u32,
    pub midi: u32,
    pub float: u32,
    pub int: u32,
    pub event_transfer: u32,
    pub min_block: u32,
    pub max_block: u32,
    pub nominal_block: u32,
    pub seq_size: u32,
    pub sample_rate: u32,
    pub update_rate: u32,
    pub scale_factor: u32,
    pub log_error: u32,
    pub log_warning: u32,
}

pub(crate) fn urids() -> &'static Urids {
    static U: OnceLock<Urids> = OnceLock::new();
    U.get_or_init(|| Urids {
        sequence: map(ATOM_SEQUENCE),
        chunk: map(ATOM_CHUNK),
        midi: map(MIDI_EVENT),
        float: map(ATOM_FLOAT),
        int: map(ATOM_INT),
        event_transfer: map(ATOM_EVENT_TRANSFER),
        min_block: map(BUF_MIN_BLOCK),
        max_block: map(BUF_MAX_BLOCK),
        nominal_block: map(BUF_NOMINAL_BLOCK),
        seq_size: map(BUF_SEQUENCE_SIZE),
        sample_rate: map(PARAM_SAMPLE_RATE),
        update_rate: map(UI_UPDATE_RATE),
        scale_factor: map(UI_SCALE_FACTOR),
        log_error: map(LOG_ERROR),
        log_warning: map(LOG_WARNING),
    })
}

/// Transparent wrapper making a static table of pointers shareable.
pub(crate) struct Table<T>(pub T);
// SAFETY: constant tables of function pointers and null handles.
unsafe impl<T> Sync for Table<T> {}

// --- Log -------------------------------------------------------------------

pub(crate) static LOG: Table<LV2_Log_Log> = Table(LV2_Log_Log { handle: null_mut(), printf: log_printf, vprintf: log_vprintf });

fn log_line(type_: u32, msg: &str) {
    let u = urids();
    // Errors and warnings only; notes and traces would flood the terminal.
    if type_ == u.log_error || type_ == u.log_warning {
        eprintln!("LV2: {}", msg.trim_end());
    }
}

unsafe extern "C" fn log_printf(_: *mut c_void, type_: u32, fmt: *const c_char) -> c_int {
    if fmt.is_null() {
        return 0;
    }
    // SAFETY: NUL-terminated format from the plugin (printed without its arguments).
    let s = unsafe { CStr::from_ptr(fmt) }.to_string_lossy();
    log_line(type_, &s);
    s.len() as c_int
}

#[cfg(unix)]
unsafe extern "C" fn log_vprintf(_: *mut c_void, type_: u32, fmt: *const c_char, ap: *mut c_void) -> c_int {
    unsafe extern "C" {
        // `va_list` is passed as a pointer on the Unix ABIs Ponzik runs on.
        fn vsnprintf(s: *mut c_char, n: usize, format: *const c_char, ap: *mut c_void) -> c_int;
    }
    if fmt.is_null() {
        return 0;
    }
    let mut buf = [0 as c_char; 1024];
    // SAFETY: bounded output buffer, format and arguments from the plugin.
    let n = unsafe { vsnprintf(buf.as_mut_ptr(), buf.len(), fmt, ap) };
    // SAFETY: `vsnprintf` NUL-terminates within the buffer.
    log_line(type_, &unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy());
    n
}

#[cfg(not(unix))]
unsafe extern "C" fn log_vprintf(h: *mut c_void, type_: u32, fmt: *const c_char, _: *mut c_void) -> c_int {
    // SAFETY: same contract as `printf`, arguments left out.
    unsafe { log_printf(h, type_, fmt) }
}

// --- Options ---------------------------------------------------------------

/// The instance's options and the values they point to (boxed: stable).
pub(crate) struct Options {
    pub min_block: i32,
    pub max_block: i32,
    pub nominal_block: i32,
    pub seq_size: i32,
    pub sample_rate: f32,
    pub list: [LV2_Options_Option; 6],
}

impl Options {
    pub(crate) fn new(sample_rate: f32, min_block: usize, max_block: usize, nominal: usize, seq_size: usize) -> Box<Options> {
        let none = || LV2_Options_Option { context: 0, subject: 0, key: 0, size: 0, type_: 0, value: null() };
        let mut o = Box::new(Options {
            min_block: min_block as i32,
            max_block: max_block as i32,
            nominal_block: nominal as i32,
            seq_size: seq_size as i32,
            sample_rate,
            list: [none(), none(), none(), none(), none(), none()],
        });
        let u = urids();
        let opt = |key: u32, type_: u32, value: *const c_void| LV2_Options_Option {
            context: LV2_OPTIONS_INSTANCE,
            subject: 0,
            key,
            size: 4,
            type_,
            value,
        };
        o.list = [
            opt(u.min_block, u.int, (&o.min_block as *const i32).cast()),
            opt(u.max_block, u.int, (&o.max_block as *const i32).cast()),
            opt(u.nominal_block, u.int, (&o.nominal_block as *const i32).cast()),
            opt(u.seq_size, u.int, (&o.seq_size as *const i32).cast()),
            opt(u.sample_rate, u.float, (&o.sample_rate as *const f32).cast()),
            none(),
        ];
        o
    }
}

// --- Worker ----------------------------------------------------------------

/// Size of the worker queues, and of the largest message.
const WORKER_BYTES: usize = 1 << 16;

pub(crate) struct WorkerShared {
    requests: ByteRing,
    responses: ByteRing,
    instance: AtomicPtr<c_void>,
    iface: AtomicPtr<LV2_Worker_Interface>,
    busy: AtomicBool,
    quit: AtomicBool,
    thread: OnceLock<Thread>,
}

/// `worker:schedule` and the thread that runs the plugin's `work`.
pub(crate) struct Worker {
    shared: Arc<WorkerShared>,
    join: Option<JoinHandle<()>>,
    pub schedule: Box<LV2_Worker_Schedule>,
    /// Responses read here on the audio thread (no allocation).
    scratch: Box<[u8]>,
}

unsafe extern "C" fn schedule_work(handle: *mut c_void, size: u32, data: *const c_void) -> u32 {
    // SAFETY: `handle` is the worker's `WorkerShared`, alive as long as the instance.
    let Some(w) = (unsafe { (handle as *const WorkerShared).as_ref() }) else { return LV2_WORKER_ERR_UNKNOWN };
    let body: &[u8] = if data.is_null() || size == 0 {
        &[]
    } else {
        // SAFETY: `size` bytes from the plugin.
        unsafe { std::slice::from_raw_parts(data.cast(), size as usize) }
    };
    if !w.requests.push(&[body]) {
        return LV2_WORKER_ERR_NO_SPACE;
    }
    if let Some(t) = w.thread.get() {
        t.unpark();
    }
    LV2_WORKER_SUCCESS
}

unsafe extern "C" fn respond(handle: *mut c_void, size: u32, data: *const c_void) -> u32 {
    // SAFETY: as in `schedule_work`.
    let Some(w) = (unsafe { (handle as *const WorkerShared).as_ref() }) else { return LV2_WORKER_ERR_UNKNOWN };
    let body: &[u8] = if data.is_null() || size == 0 {
        &[]
    } else {
        // SAFETY: `size` bytes from the plugin.
        unsafe { std::slice::from_raw_parts(data.cast(), size as usize) }
    };
    if w.responses.push(&[body]) { LV2_WORKER_SUCCESS } else { LV2_WORKER_ERR_NO_SPACE }
}

fn worker_loop(w: Arc<WorkerShared>) {
    let mut buf = vec![0u8; WORKER_BYTES];
    while !w.quit.load(Ordering::Acquire) {
        while let Some(n) = w.requests.pop(&mut buf) {
            w.busy.store(true, Ordering::SeqCst);
            let inst = w.instance.load(Ordering::SeqCst);
            let iface = w.iface.load(Ordering::SeqCst);
            // SAFETY: instance and interface stay valid while `busy` is set
            // (see `Worker::detach`).
            if !inst.is_null()
                && let Some(i) = unsafe { iface.as_ref() }
            {
                unsafe { (i.work)(inst, respond, Arc::as_ptr(&w) as *mut c_void, n as u32, buf.as_ptr().cast()) };
            }
            w.busy.store(false, Ordering::SeqCst);
        }
        std::thread::park_timeout(Duration::from_millis(100));
    }
}

impl Worker {
    pub(crate) fn new() -> Result<Worker, String> {
        let shared = Arc::new(WorkerShared {
            requests: ByteRing::new(WORKER_BYTES),
            responses: ByteRing::new(WORKER_BYTES),
            instance: AtomicPtr::new(null_mut()),
            iface: AtomicPtr::new(null_mut()),
            busy: AtomicBool::new(false),
            quit: AtomicBool::new(false),
            thread: OnceLock::new(),
        });
        let w = shared.clone();
        let join = std::thread::Builder::new()
            .name("ponzik-lv2-worker".into())
            .spawn(move || worker_loop(w))
            .map_err(|e| format!("worker thread: {e}"))?;
        let _ = shared.thread.set(join.thread().clone());
        let schedule = Box::new(LV2_Worker_Schedule { handle: Arc::as_ptr(&shared) as *mut c_void, schedule_work });
        Ok(Worker { shared, join: Some(join), schedule, scratch: vec![0u8; WORKER_BYTES].into_boxed_slice() })
    }

    /// The instance exists: requests are worked on.
    pub(crate) fn attach(&self, instance: LV2_Handle, iface: *const LV2_Worker_Interface) {
        self.shared.iface.store(iface as *mut _, Ordering::SeqCst);
        self.shared.instance.store(instance, Ordering::SeqCst);
    }

    /// The instance goes: waits for a `work` in progress, drops the queues.
    pub(crate) fn detach(&self) {
        self.shared.instance.store(null_mut(), Ordering::SeqCst);
        while self.shared.busy.load(Ordering::SeqCst) {
            std::thread::yield_now();
        }
        self.shared.responses.clear();
    }

    /// Audio thread, after `run`: the responses ready, then `end_run`.
    pub(crate) fn deliver(&mut self, instance: LV2_Handle, iface: &LV2_Worker_Interface) {
        while let Some(n) = self.shared.responses.pop(&mut self.scratch) {
            // SAFETY: instance alive, `n` bytes of the response.
            unsafe { (iface.work_response)(instance, n as u32, self.scratch.as_ptr().cast()) };
        }
        if let Some(end) = iface.end_run {
            // SAFETY: idem.
            unsafe { end(instance) };
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.detach();
        self.shared.quit.store(true, Ordering::Release);
        if let Some(j) = self.join.take() {
            j.thread().unpark();
            let _ = j.join();
        }
    }
}

// --- Shared with the editor --------------------------------------------------

const GATE_FREE: u8 = 0;
const GATE_AUDIO: u8 = 1;
const GATE_UI: u8 = 2;

/// Capacity of the UI → audio queue of control values.
pub(crate) const UI_QUEUE: usize = 1024;
/// Size of the atom queues between the plugin and its interface.
pub(crate) const UI_ATOM_BYTES: usize = 1 << 17;

pub(crate) struct Inner {
    /// Null once the instance is gone.
    pub instance: LV2_Handle,
    pub descriptor: *const LV2_Descriptor,
    pub ui: Option<crate::ui::UiInstance>,
}

pub(crate) struct Shared {
    pub model: Arc<crate::plugin::Model>,
    gate: AtomicU8,
    /// Control values of every port (bits of an `f32`), as the audio thread last saw them.
    pub values: Box<[AtomicU32]>,
    pub sample_rate: AtomicU32,
    pub inner: Mutex<Inner>,
    /// What the interface asked since the last `idle`.
    pub events: Mutex<Vec<EditorEvent>>,
    /// Control values written by the interface, to the audio thread.
    pub to_audio: Mutex<Producer<(u32, f32)>>,
    /// Interface → plugin atoms: port, type, body.
    pub atoms_in: ByteRing,
    /// Plugin → interface atoms: port, then the atom (header and body).
    pub atoms_out: ByteRing,
    /// An interface is open: the plugin's output atoms are sent to it.
    pub ui_open: AtomicBool,
}

// SAFETY: the raw pointers are only used under `inner` (UI thread) or by the
// audio thread inside the gate.
unsafe impl Send for Shared {}
unsafe impl Sync for Shared {}

pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Shared {
    pub(crate) fn new(model: Arc<crate::plugin::Model>, ports: usize) -> (Arc<Shared>, Consumer<(u32, f32)>) {
        let (tx, rx) = ponzik_engine::ring::channel(UI_QUEUE);
        let shared = Shared {
            model,
            gate: AtomicU8::new(GATE_FREE),
            values: (0..ports).map(|_| AtomicU32::new(0)).collect(),
            sample_rate: AtomicU32::new(48_000f32.to_bits()),
            inner: Mutex::new(Inner { instance: null_mut(), descriptor: null(), ui: None }),
            events: Mutex::new(Vec::new()),
            to_audio: Mutex::new(tx),
            atoms_in: ByteRing::new(UI_ATOM_BYTES),
            atoms_out: ByteRing::new(UI_ATOM_BYTES),
            ui_open: AtomicBool::new(false),
        };
        (Arc::new(shared), rx)
    }

    /// Audio thread: may `run` go? Never waits.
    pub(crate) fn try_enter_audio(&self) -> bool {
        self.gate.compare_exchange(GATE_FREE, GATE_AUDIO, Ordering::Acquire, Ordering::Relaxed).is_ok()
    }

    pub(crate) fn leave_audio(&self) {
        self.gate.store(GATE_FREE, Ordering::Release);
    }

    /// UI thread: waits for the `run` in progress (one block at most), and
    /// keeps the next ones out until [`Shared::leave_ui`].
    pub(crate) fn enter_ui(&self) {
        while self.gate.compare_exchange(GATE_FREE, GATE_UI, Ordering::Acquire, Ordering::Relaxed).is_err() {
            std::thread::yield_now();
        }
    }

    pub(crate) fn leave_ui(&self) {
        self.gate.store(GATE_FREE, Ordering::Release);
    }

    pub(crate) fn value(&self, port: u32) -> f32 {
        self.values.get(port as usize).map_or(0.0, |v| f32::from_bits(v.load(Ordering::Relaxed)))
    }

    pub(crate) fn sample_rate(&self) -> f32 {
        f32::from_bits(self.sample_rate.load(Ordering::Relaxed))
    }

    pub(crate) fn push(&self, e: EditorEvent) {
        lock(&self.events).push(e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_point_to_their_values() {
        let o = Options::new(44_100.0, 1, 4096, 512, 8192);
        let u = urids();
        assert_eq!(o.list[4].key, u.sample_rate);
        // SAFETY: the value pointers point into the boxed options.
        unsafe {
            assert_eq!(*o.list[1].value.cast::<i32>(), 4096);
            assert_eq!(*o.list[4].value.cast::<f32>(), 44_100.0);
        }
        assert_eq!(o.list[5].key, 0, "terminated by a zeroed option");
    }
}
