//! `ponzik bench [project] [seconds]`: measured costs of a project, offline.
//!
//! The engine renders the project stopped then playing, in blocks of the
//! audio callback's size, and each block's time is compared with its budget
//! (the real time it stands for). The most expensive tracks, inserts and
//! instruments follow, then the time of one interface image per view and the
//! process memory. See `docs/PERFORMANCE.md`.

use std::time::{Duration, Instant};

use ponzik_engine::{Command, PlayMode};
use ponzik_project::Project;

use crate::gui;

const SR: u32 = 48_000;
/// Frames per audio callback: 5 ms at 48 kHz, the default period under ALSA.
const BLOCK: usize = 240;

/// Project named on the command line: a demo, `empty`, or a project file.
pub fn project_named(name: Option<&str>) -> Result<Project, String> {
    Ok(match name {
        None | Some("house") => ponzik_project::demo_project(),
        Some("breakcore") => ponzik_project::demo_breakcore(),
        Some("zalgo") => ponzik_project::demo_zalgo(),
        Some("empty") => Project::empty(),
        Some(path) => ponzik_project::load_from_file(path)?,
    })
}

/// Block times of one phase, in seconds.
struct Blocks {
    times: Vec<f64>,
}

impl Blocks {
    fn report(&mut self, label: &str) {
        let budget = BLOCK as f64 / SR as f64;
        self.times.sort_by(f64::total_cmp);
        let n = self.times.len().max(1);
        let mean = self.times.iter().sum::<f64>() / n as f64;
        let p99 = self.times[(n * 99 / 100).min(n - 1)];
        let max = self.times.last().copied().unwrap_or(0.0);
        let over = self.times.iter().filter(|&&t| t > budget).count();
        println!(
            "audio {label:<8} block {BLOCK} ({:.2} ms budget) · mean {:.3} ms ({:.1} %) · p99 {:.3} ms · max {:.3} ms · over budget {over}/{n}",
            budget * 1e3,
            mean * 1e3,
            mean / budget * 100.0,
            p99 * 1e3,
            max * 1e3,
        );
    }
}

pub fn bench(args: &[String]) -> Result<(), String> {
    let project = project_named(args.first().map(String::as_str))?;
    let seconds: f64 = match args.get(1) {
        Some(s) => s.parse().map_err(|_| format!("invalid duration: {s}"))?,
        None => 8.0,
    };
    let started = Instant::now();
    let samples = gui::project_samples(&project);
    let loaded = started.elapsed();
    let mut playing = project.clone();
    if !playing.clips.is_empty() || !playing.audio_clips.is_empty() {
        playing.mode = PlayMode::Song;
    }
    let (mut engine, mut handle) = playing.build_engine(SR as f32, &|p| samples.get(p).cloned());
    engine.set_worker_threads(ponzik_engine::default_worker_threads());
    engine.set_adaptive_threads(true);
    // As in the interface: idle inserts sleep (PONZIK_SMART_DISABLE=0 turns it off).
    engine.set_smart_disable(std::env::var("PONZIK_SMART_DISABLE").map_or(true, |v| v.trim() != "0"));
    println!(
        "{} · {} channels · {} tracks · samples loaded in {:.1} ms · engine built in {:.1} ms · RSS {}",
        project.name,
        project.channels.len(),
        project.tracks.len(),
        loaded.as_secs_f64() * 1e3,
        (started.elapsed() - loaded).as_secs_f64() * 1e3,
        rss().unwrap_or_default()
    );

    let blocks = ((seconds * SR as f64) as usize / BLOCK).max(1);
    let mut out = vec![0.0f32; BLOCK * 2];
    let mut run = |engine: &mut ponzik_engine::Engine, handle: &mut ponzik_engine::EngineHandle, n: usize| {
        let mut times = Vec::with_capacity(n);
        let channels = project.channels.len();
        let tracks = project.tracks.len();
        let mut chan = vec![0.0f64; channels];
        let mut track = vec![0.0f64; tracks];
        let mut insert = vec![0.0f64; tracks * ponzik_engine::mixer::INSERT_SLOTS];
        for _ in 0..n {
            let t = Instant::now();
            engine.process(&mut out, 2);
            times.push(t.elapsed().as_secs_f64());
            handle.collect_garbage();
            for (c, v) in chan.iter_mut().enumerate() {
                *v += f64::from(handle.channel_cpu(c));
            }
            for (t, v) in track.iter_mut().enumerate() {
                *v += f64::from(handle.track_cpu(t));
            }
            for (i, v) in insert.iter_mut().enumerate() {
                *v += f64::from(handle.insert_cpu(i / ponzik_engine::mixer::INSERT_SLOTS, i % ponzik_engine::mixer::INSERT_SLOTS));
            }
        }
        (Blocks { times }, chan, track, insert)
    };

    // Stopped: what the project costs while nothing plays (tails, idle effects).
    let (mut stopped, chan, track, insert) = run(&mut engine, &mut handle, blocks / 2);
    stopped.report("stopped");
    heaviest(&project, "stopped", blocks / 2, &chan, &track, &insert);
    let _ = handle.send(Command::Play);
    let (mut play, chan, track, insert) = run(&mut engine, &mut handle, blocks);
    play.report("playing");
    heaviest(&project, "playing", blocks, &chan, &track, &insert);
    let _ = handle.send(Command::Stop);

    // Offline render speed: the whole engine as fast as it goes.
    let total: f64 = play.times.iter().sum::<f64>();
    println!("realtime factor while playing: {:.1}×", (blocks * BLOCK) as f64 / SR as f64 / total.max(1e-9));
    drop(engine);
    handle.collect_garbage();

    for view in ["playlist", "piano", "mixer", "rack"] {
        let f = gui::bench_view(project.clone(), view, 1600, 960, 60)?;
        println!(
            "ui {view:<8} 1600×960 · full image {:.2} ms · tracked frame {:.2} ms (ui {:.2} + raster {:.2}) · idle frame {:.3} ms · {} allocs/frame",
            f.full_ms,
            f.ui_ms + f.raster_ms,
            f.ui_ms,
            f.raster_ms,
            f.idle_ms,
            f.allocs
        );
    }
    println!("RSS after the views: {}", rss().unwrap_or_default());
    Ok(())
}

/// The heaviest parts of a phase, in % of one core.
fn heaviest(project: &Project, label: &str, blocks: usize, chan: &[f64], track: &[f64], insert: &[f64]) {
    use ponzik_engine::mixer::INSERT_SLOTS;
    let n = blocks.max(1) as f64;
    let mut costs: Vec<(f64, String)> = Vec::new();
    for (c, v) in chan.iter().enumerate() {
        costs.push((v / n, format!("channel {c} {} ({})", project.channels[c].name, project.channels[c].instrument.kind)));
    }
    for (t, v) in track.iter().enumerate() {
        costs.push((v / n, format!("track {t} {}", project.tracks[t].name)));
    }
    for (i, v) in insert.iter().enumerate() {
        let (t, s) = (i / INSERT_SLOTS, i % INSERT_SLOTS);
        let kind = project.tracks[t].inserts.get(s).and_then(Option::as_ref).map_or("?", |fx| fx.kind.as_str());
        costs.push((v / n, format!("  insert {t}:{s} {kind}")));
    }
    costs.sort_by(|a, b| b.0.total_cmp(&a.0));
    println!("heaviest {label} (% of one core):");
    for (v, name) in costs.iter().take(10).filter(|c| c.0 >= 0.0001) {
        println!("  {:6.2} %  {name}", v * 100.0);
    }
}

/// Resident memory of this process (Linux), for the reports.
pub fn rss() -> Option<String> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let kb: u64 = status.lines().find_map(|l| l.strip_prefix("VmRSS:"))?.trim().trim_end_matches("kB").trim().parse().ok()?;
    Some(format!("{:.1} MiB", kb as f64 / 1024.0))
}

/// Interface timing of one view, in milliseconds per image.
pub struct FrameTimes {
    pub full_ms: f64,
    pub ui_ms: f64,
    pub raster_ms: f64,
    /// A frame where nothing changed (same input, transport stopped).
    pub idle_ms: f64,
    pub allocs: u64,
}

pub fn ms(d: Duration, n: usize) -> f64 {
    d.as_secs_f64() * 1e3 / n.max(1) as f64
}
