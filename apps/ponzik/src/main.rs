mod demo;
mod gui;

use std::io::{self, BufRead, Write};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use ponzik_engine::wav::write_wav_i16;
use ponzik_engine::{Channel, Command, Engine};

const HELP: &str = "\
Ponzik — le DAW libre. No more gatekeeping.

Usage :
  ponzik                         ouvre l'interface
  ponzik play [secondes]         joue le beat de démo dans le terminal (Entrée pour arrêter)
  ponzik render <sortie.wav> [mesures]
                                 exporte le beat de démo (4 mesures par défaut)
  ponzik screenshot <sortie.png> [largeur hauteur]
                                 capture l'interface hors écran, en lecture
  ponzik help                    affiche cette aide";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        None => gui::run(),
        Some("play") => play(args.get(1).map(String::as_str)),
        Some("screenshot") => screenshot(&args[1..]),
        Some("render") => render(&args[1..]),
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
    let rack = demo::rack();
    let names: Vec<String> = rack.iter().map(|c| c.name.clone()).collect();
    let grid = step_grid(&rack);
    let (mut engine, mut handle) = Engine::new(config.sample_rate as f32, rack);
    send(&mut handle, Command::SetBpm(demo::BPM));
    send(&mut handle, Command::Play);

    let stream = output.start(move |out, channels| engine.process(out, channels)).map_err(|e| e.to_string())?;
    let cfg = stream.config();
    println!("🎛  Ponzik · {} BPM · {} Hz · {} canaux · {}", demo::BPM, cfg.sample_rate, cfg.channels, stream.device_name());
    match seconds {
        Some(s) => println!("Lecture pendant {s} s…\n"),
        None => println!("Entrée pour arrêter.\n"),
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
    let mut first = true;
    loop {
        let timed_out = seconds.is_some_and(|s| started.elapsed().as_secs_f64() >= s);
        if timed_out || stop.load(Ordering::Relaxed) {
            break;
        }
        if !stream.is_running() {
            return Err("le pilote audio s'est arrêté".into());
        }
        draw_rack(&names, &grid, handle.playhead(), first);
        first = false;
        thread::sleep(Duration::from_millis(30));
    }

    send(&mut handle, Command::Stop);
    thread::sleep(Duration::from_millis(150));
    drop(stream);
    Ok(())
}

fn render(args: &[String]) -> Result<(), String> {
    let path = args.first().ok_or_else(|| format!("il manque le fichier de sortie\n\n{HELP}"))?;
    let bars: u32 = match args.get(1) {
        Some(b) => b.parse().ok().filter(|&b| b > 0).ok_or_else(|| format!("nombre de mesures invalide : {b}"))?,
        None => 4,
    };

    const SAMPLE_RATE: u32 = 48_000;
    const CHANNELS: usize = 2;
    let (mut engine, mut handle) = Engine::new(SAMPLE_RATE as f32, demo::rack());
    send(&mut handle, Command::SetBpm(demo::BPM));
    send(&mut handle, Command::Play);

    let music = (engine.samples_per_step() * 16.0 * bars as f64).round() as usize;
    let tail = SAMPLE_RATE as usize; // une seconde pour laisser mourir les queues
    let mut samples = vec![0.0f32; (music + tail) * CHANNELS];
    let (body, rest) = samples.split_at_mut(music * CHANNELS);
    for block in body.chunks_mut(512 * CHANNELS) {
        engine.process(block, CHANNELS);
    }
    send(&mut handle, Command::Stop);
    // Stop coupe net ; on enchaîne quand même le rendu pour un fichier propre.
    for block in rest.chunks_mut(512 * CHANNELS) {
        engine.process(block, CHANNELS);
    }

    write_wav_i16(path, SAMPLE_RATE, CHANNELS as u16, &samples).map_err(|e| format!("{path} : {e}"))?;
    let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    println!(
        "✅ {path} : {bars} mesures à {} BPM, {:.1} s, crête {:.1} dBFS",
        demo::BPM,
        samples.len() as f64 / (SAMPLE_RATE as f64 * CHANNELS as f64),
        20.0 * peak.max(1e-9).log10()
    );
    Ok(())
}

fn screenshot(args: &[String]) -> Result<(), String> {
    let path = args.first().ok_or_else(|| format!("il manque le fichier de sortie\n\n{HELP}"))?;
    let dim = |i: usize, default: usize| -> Result<usize, String> {
        args.get(i).map_or(Ok(default), |v| v.parse().map_err(|_| format!("dimension invalide : {v}")))
    };
    let (w, h) = (dim(1, 1600)?, dim(2, 960)?);
    gui::screenshot(path, w, h)?;
    println!("✅ {path} : {w}×{h}");
    Ok(())
}

fn send(handle: &mut ponzik_engine::EngineHandle, cmd: Command) {
    if handle.send(cmd).is_err() {
        eprintln!("file de commandes pleine, commande ignorée : {cmd:?}");
    }
}

fn step_grid(rack: &[Channel]) -> Vec<[bool; 16]> {
    rack.iter().map(|c| std::array::from_fn(|i| c.steps[i].is_some())).collect()
}

/// Channel Rack en ASCII, redessiné sur place.
fn draw_rack(names: &[String], grid: &[[bool; 16]], playhead: Option<usize>, first: bool) {
    let mut out = String::new();
    if !first {
        out.push_str(&format!("\x1b[{}A", names.len() + 1));
    }
    for (name, steps) in names.iter().zip(grid) {
        out.push_str(&format!("  {name:<9} "));
        for (i, &on) in steps.iter().enumerate() {
            if i % 4 == 0 {
                out.push(' ');
            }
            out.push_str(match (on, playhead == Some(i)) {
                (true, true) => "\x1b[1;93m■\x1b[0m",
                (true, false) => "\x1b[33m■\x1b[0m",
                (false, true) => "\x1b[97m□\x1b[0m",
                (false, false) => "\x1b[90m·\x1b[0m",
            });
        }
        out.push('\n');
    }
    out.push_str("            ");
    for i in 0..16 {
        if i % 4 == 0 {
            out.push(' ');
        }
        out.push(if playhead == Some(i) { '▲' } else { ' ' });
    }
    out.push('\n');
    let mut stdout = io::stdout().lock();
    let _ = stdout.write_all(out.as_bytes());
    let _ = stdout.flush();
}
