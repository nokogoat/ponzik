mod alloc_count;
mod build_info;
mod gui;
mod install;
#[cfg(test)]
mod i18n_coverage;

use std::io::{self, BufRead};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use ponzik_engine::{Command, PlayMode};
use ponzik_i18n::{tr, tr_fmt};
use ponzik_project::{BitDepth, RenderMode, demo_project};

#[global_allocator]
static ALLOCATOR: alloc_count::Counting = alloc_count::Counting;

fn help() -> &'static str {
    tr("\
Ponzik — the free DAW. No more gatekeeping.

Usage:
  ponzik                                   opens the interface (house demo)
  ponzik breakcore                         opens the interface on the breakcore demo
  ponzik zalgo                             opens the interface on the rage × techno demo
  ponzik play [seconds]                    plays the demo in the terminal (Enter to stop)
  ponzik render <output.wav> [song|pattern] [breakcore|zalgo]
                                           exports a demo as a 24-bit WAV
  ponzik screenshot <output.png> [view] [width height]
                                           captures the interface off-screen
                                           (views: rack, piano, playlist, mixer, lab;
                                           bc-<view>, zg-<view>: the other demos;
                                           PONZIK_SAMPLELAB=demo|<wav> opens the Sample Lab;
                                           PONZIK_SCALE_UI=2 renders at 2× for HiDPI;
                                           PONZIK_LANG=fr picks the language)
  ponzik library [dir]                     renders the bundled sound library (24-bit WAV)
                                           into dir (default: ~/Music/Ponzik/Library)
  ponzik check-update [nightly|stable]     asks GitHub for a newer release
  ponzik install [--desktop]               Windows: installs for this user (Start menu,
                                           file types, Installed apps; --desktop: a
                                           desktop shortcut too); Linux: adds the
                                           AppImage to the applications menu
  ponzik uninstall [--yes] [--purge]       removes what install added (--purge: the
                                           settings too; projects always stay)
  ponzik <file.ponzik | file.flp>          opens a project
  ponzik version                           shows the version and build
  ponzik help                              shows this help")
}

fn main() -> ExitCode {
    gui::init_language();
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Internal: loads one plugin for the scan, in this throwaway process.
    if args.first().is_some_and(|a| a == "scan-plugin") {
        return scan_plugin(args.get(1).map(String::as_str));
    }
    // Windows: the program is a console program (the command line prints), but
    // started from the Explorer it only owns a console of its own: let it go.
    #[cfg(windows)]
    if args.first().is_none_or(|a| a == "breakcore" || a == "zalgo" || a == "--uninstall" || project_arg(a).is_some()) {
        detach_own_console();
    }
    let result = match args.first().map(String::as_str) {
        None => gui::run(demo_project(), None),
        Some("breakcore") => gui::run(ponzik_project::demo_breakcore(), None),
        Some("zalgo") => gui::run(ponzik_project::demo_zalgo(), None),
        Some(a) if project_arg(a).is_some() => gui::run(demo_project(), project_arg(a)),
        Some("install") => cli_install(&args[1..]),
        Some("uninstall") => cli_uninstall(&args[1..]),
        // "Installed apps" › Uninstall: a confirmation window (none with --quiet).
        Some("--uninstall") if args.iter().any(|a| a == "--quiet") => uninstall_now(false),
        Some("--uninstall") => gui::run_uninstall(),
        Some("play") => play(args.get(1).map(String::as_str)),
        Some("render") => render(&args[1..]),
        Some("screenshot") => screenshot(&args[1..]),
        Some("library") => library(args.get(1).map(String::as_str)),
        Some("version" | "--version" | "-V") => {
            println!("Ponzik {}", build_info::describe());
            Ok(())
        }
        Some("check-update") => check_update(args.get(1).map(String::as_str)),
        Some("help" | "-h" | "--help") => {
            println!("{}", help());
            Ok(())
        }
        Some(other) => Err(tr_fmt!("unknown command: {}\n\n{}", other, help())),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{}", tr_fmt!("error: {}", e.as_str()));
            ExitCode::FAILURE
        }
    }
}

/// A project file given on the command line (a double-click in the file
/// manager): an existing `.ponzik` or `.flp`.
fn project_arg(arg: &str) -> Option<std::path::PathBuf> {
    let path = std::path::Path::new(arg);
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    (matches!(ext.as_str(), "ponzik" | "flp") && path.is_file()).then(|| path.to_path_buf())
}

/// `ponzik install [--desktop]`: installs (Windows) or adds to the menu (Linux), without a window.
fn cli_install(args: &[String]) -> Result<(), String> {
    let opts = install::Options { desktop: args.iter().any(|a| a == "--desktop"), move_to_applications: false };
    let st = install::status();
    if st.platform == install::Platform::Linux && st.place == install::Place::Unmanaged {
        eprintln!("{}", tr("Not started from an AppImage: the menu entry will start this program file"));
    }
    let report = install::install(opts)?;
    for w in &report.warnings {
        eprintln!("{}", tr_fmt!("warning: {}", w.as_str()));
    }
    println!("{}", tr_fmt!("Ponzik is installed: {}", report.program.display().to_string()));
    Ok(())
}

/// `ponzik uninstall [--yes] [--purge]`: asks first, unless `--yes`.
fn cli_uninstall(args: &[String]) -> Result<(), String> {
    let purge = args.iter().any(|a| a == "--purge");
    if !args.iter().any(|a| a == "--yes" || a == "-y") {
        let what = if purge { tr("Remove Ponzik and its settings? Your projects stay. [y/N] ") } else { tr("Remove Ponzik? Your projects and settings stay. [y/N] ") };
        eprint!("{what}");
        let mut answer = String::new();
        let _ = io::stdin().lock().read_line(&mut answer);
        let a = answer.trim().to_lowercase();
        if !(a.starts_with('y') || a.starts_with('o')) {
            println!("{}", tr("Nothing was removed"));
            return Ok(());
        }
    }
    uninstall_now(purge)
}

/// Uninstalls, then (Windows) deletes the program once this process ends.
fn uninstall_now(purge: bool) -> Result<(), String> {
    let warnings = install::uninstall(purge, &gui::config_dir())?;
    for w in &warnings {
        eprintln!("{}", tr_fmt!("warning: {}", w.as_str()));
    }
    let done = match install::Platform::current() {
        install::Platform::Linux => tr("Ponzik is no longer in the applications menu. The AppImage stays where it is."),
        _ => tr("Ponzik is uninstalled. Your projects are still there."),
    };
    println!("{done}");
    install::delete_program_after_exit()
}

/// Frees the console window Windows opened for this process alone (started
/// from the Explorer); a console shared with a terminal stays.
#[cfg(windows)]
fn detach_own_console() {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetConsoleProcessList(list: *mut u32, count: u32) -> u32;
        fn FreeConsole() -> i32;
    }
    let mut ids = [0u32; 4];
    // SAFETY: the buffer holds `ids.len()` process ids.
    let n = unsafe { GetConsoleProcessList(ids.as_mut_ptr(), ids.len() as u32) };
    if n == 1 {
        // SAFETY: no handle of the console is kept; writes to it fail quietly afterwards.
        unsafe { FreeConsole() };
    }
}

/// `ponzik check-update [nightly|stable]`: the release of the channel, and
/// whether it is newer than this build.
fn check_update(channel: Option<&str>) -> Result<(), String> {
    use ponzik_project::update::{self, Channel};
    let channel = match channel {
        None => Channel::Nightly,
        Some(c) => Channel::from_id(c).ok_or_else(|| tr_fmt!("unknown channel: {}", c))?,
    };
    let token = std::env::var("PONZIK_GITHUB_TOKEN").ok().filter(|t| !t.trim().is_empty());
    let scratch = std::env::temp_dir().join(format!("ponzik-check-{}.json", std::process::id()));
    let release = update::fetch_release(channel, token.as_deref(), &scratch).map_err(|e| e.message())?;
    let current = build_info::version();
    let Some(latest) = release.version() else {
        return Err(tr_fmt!("{}: no version in the release", release.tag.as_str()));
    };
    println!("{}: {}", tr("This build"), build_info::describe());
    println!("{} ({}): {} · {}", tr("Latest release"), channel.id(), latest.id(), release.published_at);
    let msg = if latest.is_newer_than(&current) { tr("An update is available") } else { tr("Ponzik is up to date") };
    println!("{msg}");
    Ok(())
}

/// `ponzik scan-plugin <path>`: what the plugin file holds, on standard output
/// (see `ponzik_project::plugins`). Run by the scan in a child process, so a
/// plugin that crashes or hangs only takes this process down.
fn scan_plugin(path: Option<&str>) -> ExitCode {
    use std::io::Write;
    let Some(path) = path else {
        eprintln!("usage: ponzik scan-plugin <file.clap | bundle.vst3>");
        return ExitCode::from(2);
    };
    let (out, ok) = ponzik_project::plugins::scan_child(std::path::Path::new(path));
    let mut stdout = io::stdout().lock();
    let _ = stdout.write_all(out.as_bytes()).and_then(|_| stdout.flush());
    if ok { ExitCode::SUCCESS } else { ExitCode::from(2) }
}

fn play(seconds: Option<&str>) -> Result<(), String> {
    let seconds = seconds.map(|s| s.parse::<f64>().map_err(|_| tr_fmt!("invalid duration: {}", s))).transpose()?;
    let output = ponzik_audio::open_default().map_err(|e| e.to_string())?;
    let config = output.config();
    let mut project = demo_project();
    project.mode = PlayMode::Song;
    let none = |_: &str| None;
    let (mut engine, mut handle) = project.build_engine(config.sample_rate as f32, &none);
    engine.set_worker_threads(ponzik_engine::default_worker_threads());
    engine.set_adaptive_threads(true);
    let _ = handle.send(Command::Play);
    let stream = output.start(move |out, ch| engine.process(out, ch)).map_err(|e| e.to_string())?;
    println!("Ponzik · {} · {} BPM · {} Hz · {}", project.name, project.bpm, config.sample_rate, stream.device_name());
    match seconds {
        Some(s) => println!("{}", tr_fmt!("Playing for {} s…", s)),
        None => println!("{}", tr("Press Enter to stop.")),
    }

    let stop = Arc::new(AtomicBool::new(false));
    if seconds.is_none() {
        let stop = stop.clone();
        thread::spawn(move || {
            let _ = io::stdin().lock().lines().next();
            stop.store(true, Ordering::Relaxed);
        });
    }
    let started = Instant::now();
    loop {
        let timed_out = seconds.is_some_and(|s| started.elapsed().as_secs_f64() >= s);
        if timed_out || stop.load(Ordering::Relaxed) || !stream.is_running() {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    let _ = handle.send(Command::Stop);
    thread::sleep(Duration::from_millis(150));
    Ok(())
}

fn render(args: &[String]) -> Result<(), String> {
    let path = args.first().ok_or_else(|| tr_fmt!("missing output file\n\n{}", help()))?;
    let project = match args.get(2).map(String::as_str) {
        Some("breakcore") => ponzik_project::demo_breakcore(),
        Some("zalgo") => ponzik_project::demo_zalgo(),
        _ => demo_project(),
    };
    let mode = match args.get(1).map(String::as_str) {
        None | Some("song" | "morceau") => RenderMode::Song,
        Some("pattern") => RenderMode::Pattern(0, 4),
        Some(other) => return Err(tr_fmt!("unknown mode: {}", other)),
    };
    const SR: u32 = 48_000;
    let samples = gui::project_samples(&project);
    let audio = ponzik_project::render_song(&project, SR, &|p| samples.get(p).cloned(), mode);
    ponzik_project::write_wav(path, &audio, SR, BitDepth::Pcm24).map_err(|e| format!("{path}: {e}"))?;
    let peak = audio.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    println!(
        "{}",
        tr_fmt!(
            "{}: {:.1} s, peak {:.1} dBFS",
            path.as_str(),
            audio.len() as f64 / (2.0 * SR as f64),
            20.0 * peak.max(1e-9).log10()
        )
    );
    Ok(())
}

fn library(dir: Option<&str>) -> Result<(), String> {
    let dir = dir.map_or_else(gui::library_dir, std::path::PathBuf::from);
    let started = Instant::now();
    let threads = thread::available_parallelism().map_or(4, |n| n.get());
    let progress = |done: usize, total: usize| eprint!("\r{done}/{total}");
    let count = ponzik_project::library::generate_library(&dir, threads, &progress)?;
    eprintln!();
    println!("{}: {count} sounds in {:.1} s", dir.display(), started.elapsed().as_secs_f32());
    Ok(())
}

fn screenshot(args: &[String]) -> Result<(), String> {
    let path = args.first().ok_or_else(|| tr_fmt!("missing output file\n\n{}", help()))?;
    let view = args.get(1).map_or("rack", String::as_str);
    let dim = |i: usize, default: usize| -> Result<usize, String> {
        args.get(i).map_or(Ok(default), |v| v.parse().map_err(|_| tr_fmt!("invalid dimension: {}", v.as_str())))
    };
    let (w, h) = (dim(2, 1600)?, dim(3, 960)?);
    gui::screenshot(path, w, h, view)?;
    println!("{}", tr_fmt!("{}: {}×{}, view {}", path.as_str(), w, h, view));
    Ok(())
}
