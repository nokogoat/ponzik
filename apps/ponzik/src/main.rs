mod gui;

use std::io::{self, BufRead};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use ponzik_engine::{Command, PlayMode};
use ponzik_project::{BitDepth, RenderMode, demo_project};

const HELP: &str = "\
Ponzik — le DAW libre. No more gatekeeping.

Usage :
  ponzik                                   ouvre l'interface (démo house)
  ponzik breakcore                         ouvre l'interface sur la démo breakcore
  ponzik play [secondes]                   joue la démo dans le terminal (Entrée pour arrêter)
  ponzik render <sortie.wav> [morceau|pattern]
                                           exporte la démo en WAV 24 bits
  ponzik screenshot <sortie.png> [vue] [largeur hauteur]
                                           capture l'interface hors écran
                                           (vues : rack, piano, playlist, mixer, lab ;
                                           PONZIK_SAMPLELAB=demo|<wav> ouvre l'Atelier ;
                                           PONZIK_SCALE_UI=2 rend en 2× pour l'HiDPI)
  ponzik help                              affiche cette aide";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        None => gui::run(demo_project()),
        Some("breakcore") => gui::run(ponzik_project::demo_breakcore()),
        Some("play") => play(args.get(1).map(String::as_str)),
        Some("render") => render(&args[1..]),
        Some("screenshot") => screenshot(&args[1..]),
        Some("help" | "-h" | "--help") => {
            println!("{HELP}");
            Ok(())
        }
        Some(other) => Err(format!("commande inconnue : {other}\n\n{HELP}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("erreur : {e}");
            ExitCode::FAILURE
        }
    }
}

fn play(seconds: Option<&str>) -> Result<(), String> {
    let seconds = seconds.map(|s| s.parse::<f64>().map_err(|_| format!("durée invalide : {s}"))).transpose()?;
    let output = ponzik_audio::open_default().map_err(|e| e.to_string())?;
    let config = output.config();
    let mut project = demo_project();
    project.mode = PlayMode::Song;
    let none = |_: &str| None;
    let (mut engine, mut handle) = project.build_engine(config.sample_rate as f32, &none);
    let _ = handle.send(Command::Play);
    let stream = output.start(move |out, ch| engine.process(out, ch)).map_err(|e| e.to_string())?;
    println!("Ponzik · {} · {} BPM · {} Hz · {}", project.name, project.bpm, config.sample_rate, stream.device_name());
    match seconds {
        Some(s) => println!("Lecture pendant {s} s…"),
        None => println!("Entrée pour arrêter."),
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
    let path = args.first().ok_or_else(|| format!("il manque le fichier de sortie\n\n{HELP}"))?;
    let project = if args.get(2).is_some_and(|a| a == "breakcore") { ponzik_project::demo_breakcore() } else { demo_project() };
    let mode = match args.get(1).map(String::as_str) {
        None | Some("morceau") => RenderMode::Song,
        Some("pattern") => RenderMode::Pattern(0, 4),
        Some(other) => return Err(format!("mode inconnu : {other}")),
    };
    const SR: u32 = 48_000;
    let none = |_: &str| None;
    let audio = ponzik_project::render_song(&project, SR, &none, mode);
    ponzik_project::write_wav(path, &audio, SR, BitDepth::Pcm24).map_err(|e| format!("{path} : {e}"))?;
    let peak = audio.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    println!(
        "{path} : {:.1} s, crête {:.1} dBFS",
        audio.len() as f64 / (2.0 * SR as f64),
        20.0 * peak.max(1e-9).log10()
    );
    Ok(())
}

fn screenshot(args: &[String]) -> Result<(), String> {
    let path = args.first().ok_or_else(|| format!("il manque le fichier de sortie\n\n{HELP}"))?;
    let view = args.get(1).map_or("rack", String::as_str);
    let dim = |i: usize, default: usize| -> Result<usize, String> {
        args.get(i).map_or(Ok(default), |v| v.parse().map_err(|_| format!("dimension invalide : {v}")))
    };
    let (w, h) = (dim(2, 1600)?, dim(3, 960)?);
    gui::screenshot(path, w, h, view)?;
    println!("{path} : {w}×{h}, vue {view}");
    Ok(())
}
