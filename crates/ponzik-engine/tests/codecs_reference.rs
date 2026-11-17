//! MP3 and Ogg Vorbis decoding checked against reference tools: files are
//! encoded at test time from synthetic signals (with `lame` and `ffmpeg` when
//! they are installed, the tests are skipped otherwise), then our decode is
//! compared with FFmpeg's.

use ponzik_engine::codecs::{self, Decoded};
use std::path::{Path, PathBuf};
use std::process::Command;

fn has(tool: &str) -> bool {
    Command::new(tool).arg(if tool == "lame" { "--version" } else { "-version" }).output().is_ok_and(|o| o.status.success())
}

/// A scratch directory, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("ponzik-codecs-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// xorshift noise.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0
    }
}

/// Interleaved test music: a chord, a chirp, noise bursts and clicks (the
/// clicks force short blocks), different on the two channels.
fn music(rate: u32, seconds: f32, channels: usize) -> Vec<f32> {
    let n = (rate as f32 * seconds) as usize;
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut out = Vec::with_capacity(n * channels);
    for i in 0..n {
        let t = i as f32 / rate as f32;
        let tau = std::f32::consts::TAU;
        let chord = (tau * 220.0 * t).sin() * 0.2 + (tau * 277.2 * t).sin() * 0.15 + (tau * 329.6 * t).sin() * 0.1;
        let chirp = (tau * (100.0 + 2_000.0 * t) * t).sin() * 0.15;
        let burst = if (t * 3.0).fract() < 0.1 { rng.next() * 0.3 } else { rng.next() * 0.01 };
        let click = if i % (rate as usize / 2) < 8 { 0.6 } else { 0.0 };
        out.push(chord + chirp + burst + click);
        if channels == 2 {
            out.push(chord * 0.5 - chirp + burst * 0.7 + (tau * 1_234.0 * t).sin() * 0.1);
        }
    }
    out
}

fn run(cmd: &mut Command) {
    let out = cmd.output().expect("tool runs");
    assert!(out.status.success(), "{cmd:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// FFmpeg's own decode, as planar channels (first two).
fn ffmpeg_decode(file: &Path, decoder: Option<&str>) -> (Vec<f32>, Vec<f32>) {
    let raw = file.with_extension("raw");
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-hide_banner", "-loglevel", "error", "-y"]);
    if let Some(d) = decoder {
        cmd.args(["-c:a", d]);
    }
    cmd.arg("-i").arg(file).args(["-f", "f32le", "-c:a", "pcm_f32le"]).arg(&raw);
    run(&mut cmd);
    let probe = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "a:0", "-show_entries", "stream=channels", "-of", "csv=p=0"])
        .arg(file)
        .output()
        .unwrap();
    let channels: usize = String::from_utf8_lossy(&probe.stdout).trim().trim_end_matches(',').parse().unwrap_or(1);
    let bytes = std::fs::read(&raw).unwrap();
    let samples: Vec<f32> = bytes.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
    let left: Vec<f32> = samples.chunks_exact(channels).map(|f| f[0]).collect();
    let right: Vec<f32> = samples.chunks_exact(channels).map(|f| f[channels.min(2) - 1]).collect();
    (left, right)
}

/// Signal-to-noise ratio of `ours` against `reference`, in dB.
fn snr(reference: &[f32], ours: &[f32]) -> f64 {
    let signal: f64 = reference.iter().map(|&v| (v as f64).powi(2)).sum();
    let noise: f64 = reference.iter().zip(ours).map(|(&a, &b)| (a as f64 - b as f64).powi(2)).sum();
    10.0 * (signal / noise.max(1e-30)).log10()
}

fn compare(name: &str, ours: &Decoded, reference: &(Vec<f32>, Vec<f32>), min_snr: f64) {
    assert_eq!(ours.frames(), reference.0.len(), "{name}: length");
    let (l, r) = (snr(&reference.0, &ours.left), snr(&reference.1, &ours.right));
    eprintln!("{name}: {} frames, SNR {l:.1} / {r:.1} dB", ours.frames());
    assert!(l > min_snr && r > min_snr, "{name}: SNR {l:.1} / {r:.1} dB");
}

fn write_wav(path: &Path, rate: u32, channels: usize, pcm: &[f32]) {
    ponzik_engine::wav::write_wav_i16(path, rate, channels as u16, pcm).unwrap();
}

#[test]
fn mp3_matches_ffmpeg() {
    if !has("ffmpeg") || !has("ffprobe") {
        eprintln!("ffmpeg missing: skipped");
        return;
    }
    let dir = Scratch::new("mp3");
    // (name, rate, channels, encoder arguments)
    let mut cases: Vec<(&str, u32, usize, Vec<&str>)> = vec![
        ("ff-cbr128-stereo", 44_100, 2, vec!["-c:a", "libmp3lame", "-b:a", "128k"]),
        ("ff-vbr-mono-48k", 48_000, 1, vec!["-c:a", "libmp3lame", "-q:a", "2"]),
        ("ff-lsf-22k", 22_050, 2, vec!["-c:a", "libmp3lame", "-b:a", "64k"]),
        ("ff-lsf-16k-mono", 16_000, 1, vec!["-c:a", "libmp3lame", "-b:a", "32k"]),
        ("ff-mpeg25-11k", 11_025, 2, vec!["-c:a", "libmp3lame", "-b:a", "32k"]),
        ("ff-mpeg25-8k-mono", 8_000, 1, vec!["-c:a", "libmp3lame", "-b:a", "16k"]),
        ("ff-32k-320", 32_000, 2, vec!["-c:a", "libmp3lame", "-b:a", "320k"]),
    ];
    let lame = has("lame");
    if lame {
        cases.push(("lame-js-v0", 44_100, 2, vec!["lame", "-V", "0"]));
        cases.push(("lame-stereo-b96", 44_100, 2, vec!["lame", "-m", "s", "-b", "96"]));
        cases.push(("lame-low-v9", 44_100, 2, vec!["lame", "-V", "9"]));
        cases.push(("lame-crc", 44_100, 2, vec!["lame", "-p", "-b", "160"]));
        cases.push(("lame-notag", 44_100, 2, vec!["lame", "-t", "-b", "128"]));
        cases.push(("lame-free-format", 44_100, 2, vec!["lame", "--freeformat", "-b", "200"]));
    }
    for (name, rate, channels, args) in cases {
        let wav = dir.path(&format!("{name}.wav"));
        let mp3 = dir.path(&format!("{name}.mp3"));
        write_wav(&wav, rate, channels, &music(rate, 2.3, channels));
        if args[0] == "lame" {
            run(Command::new("lame").arg("--quiet").args(&args[1..]).arg(&wav).arg(&mp3));
        } else {
            run(Command::new("ffmpeg").args(["-hide_banner", "-loglevel", "error", "-y", "-i"]).arg(&wav).args(&args).arg(&mp3));
        }
        let ours = codecs::mp3::decode(&std::fs::read(&mp3).unwrap()).unwrap_or_else(|e| panic!("{name}: {e}"));
        // LAME may resample low bitrates.
        assert!(ours.sample_rate == rate || args[0] == "lame", "{name}");
        if name.contains("free") {
            // FFmpeg does not read free format: LAME's own (16-bit) decoder.
            let wav = dir.path(&format!("{name}-lame.wav"));
            run(Command::new("lame").args(["--quiet", "--decode"]).arg(&mp3).arg(&wav));
            let s = ponzik_engine::wav::load_wav(&wav).unwrap();
            compare(name, &ours, &(s.left, s.right), 70.0);
        } else {
            compare(name, &ours, &ffmpeg_decode(&mp3, Some("mp3float")), 90.0);
        }
    }
}

/// The length an Ogg stream declares (its last granule position).
fn declared_frames(file: &Path) -> usize {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "a:0", "-show_entries", "stream=duration_ts", "-of", "csv=p=0"])
        .arg(file)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().trim_end_matches(',').parse().unwrap()
}

#[test]
fn vorbis_matches_libvorbis() {
    if !has("ffmpeg") || !has("ffprobe") {
        eprintln!("ffmpeg missing: skipped");
        return;
    }
    let dir = Scratch::new("vorbis");
    let surround = "pan=5.1|FL=c0|FR=c1|FC=0.5*c0+0.5*c1|LFE=0.3*c0|BL=0.7*c1|BR=0.7*c0";
    // (name, rate, channels of the source, encoder arguments)
    let cases: Vec<(&str, u32, usize, Vec<&str>)> = vec![
        ("q4-stereo", 44_100, 2, vec!["-q:a", "4"]),
        ("q-1-stereo", 44_100, 2, vec!["-q:a", "-1"]),
        ("q10-stereo-48k", 48_000, 2, vec!["-q:a", "10"]),
        ("q3-mono-22k", 22_050, 1, vec!["-q:a", "3"]),
        ("q0-mono-8k", 8_000, 1, vec!["-q:a", "0"]),
        ("q5-surround", 44_100, 2, vec!["-af", surround, "-q:a", "5"]),
        ("abr-96k", 32_000, 2, vec!["-b:a", "96k"]),
    ];
    let mut chained = Vec::new();
    for (name, rate, channels, args) in cases {
        let wav = dir.path(&format!("{name}.wav"));
        let ogg = dir.path(&format!("{name}.ogg"));
        write_wav(&wav, rate, channels, &music(rate, 2.3, channels));
        run(Command::new("ffmpeg").args(["-hide_banner", "-loglevel", "error", "-y", "-i"]).arg(&wav).args(["-c:a", "libvorbis"]).args(&args).arg(&ogg));
        let bytes = std::fs::read(&ogg).unwrap();
        let ours = codecs::ogg::decode(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(ours.sample_rate, rate, "{name}");
        assert_eq!(ours.frames(), declared_frames(&ogg), "{name}: trimmed to the last granule position");
        // FFmpeg may keep the end padding: compare what both have.
        // FFmpeg's libvorbis wrapper keeps the Vorbis channel order; its own
        // decoder puts front left and right first, as we do.
        let decoder = if args.contains(&surround) { "vorbis" } else { "libvorbis" };
        let (mut l, mut r) = ffmpeg_decode(&ogg, Some(decoder));
        assert!(l.len() >= ours.frames(), "{name}");
        l.truncate(ours.frames());
        r.truncate(ours.frames());
        compare(name, &ours, &(l, r), 110.0);
        if name.ends_with("stereo") {
            chained.push((bytes, ours));
        }
    }
    // Chained streams (files put end to end) play one after the other.
    let file: Vec<u8> = chained.iter().flat_map(|(b, _)| b.clone()).collect();
    let ours = codecs::ogg::decode(&file).unwrap();
    let left: Vec<f32> = chained.iter().flat_map(|(_, d)| d.left.clone()).collect();
    assert_eq!(ours.left, left);
}
