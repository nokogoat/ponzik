mod gui;
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

fn help() -> &'static str {
    tr("\
Ponzik — the free DAW. No more gatekeeping.

Usage:
  ponzik                                   opens the interface (house demo)
  ponzik breakcore                         opens the interface on the breakcore demo
  ponzik play [seconds]                    plays the demo in the terminal (Enter to stop)
  ponzik render <output.wav> [song|pattern]
                                           exports the demo as a 24-bit WAV
  ponzik screenshot <output.png> [view] [width height]
                                           captures the interface off-screen
                                           (views: rack, piano, playlist, mixer, lab;
                                           PONZIK_SAMPLELAB=demo|<wav> opens the Sample Lab;
                                           PONZIK_SCALE_UI=2 renders at 2× for HiDPI;
                                           PONZIK_LANG=fr picks the language)
  ponzik help                              shows this help")
}

fn main() -> ExitCode {
    gui::init_language();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        None => gui::run(demo_project()),
        Some("breakcore") => gui::run(ponzik_project::demo_breakcore()),
        Some("play") => play(args.get(1).map(String::as_str)),
        Some("render") => render(&args[1..]),
        Some("screenshot") => screenshot(&args[1..]),
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

fn play(seconds: Option<&str>) -> Result<(), String> {
    let seconds = seconds.map(|s| s.parse::<f64>().map_err(|_| tr_fmt!("invalid duration: {}", s))).transpose()?;
    let output = ponzik_audio::open_default().map_err(|e| e.to_string())?;
    let config = output.config();
    let mut project = demo_project();
    project.mode = PlayMode::Song;
    let none = |_: &str| None;
    let (mut engine, mut handle) = project.build_engine(config.sample_rate as f32, &none);
    engine.set_worker_threads(ponzik_engine::default_worker_threads());
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
    let project = if args.get(2).is_some_and(|a| a == "breakcore") { ponzik_project::demo_breakcore() } else { demo_project() };
    let mode = match args.get(1).map(String::as_str) {
        None | Some("song" | "morceau") => RenderMode::Song,
        Some("pattern") => RenderMode::Pattern(0, 4),
        Some(other) => return Err(tr_fmt!("unknown mode: {}", other)),
    };
    const SR: u32 = 48_000;
    let none = |_: &str| None;
    let audio = ponzik_project::render_song(&project, SR, &none, mode);
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
