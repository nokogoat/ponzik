//! Export hors ligne : rendu du morceau ou d'un pattern, pistes séparées (stems),
//! normalisation (crête ou sonie intégrée), écriture WAV 16 bits (dither TPDF),
//! 24 bits ou flottant 32 bits.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;

use ponzik_dsp::loudness::LoudnessMeter;
use ponzik_dsp::noise::WhiteNoise;
use ponzik_engine::{Command, PPQ, PlayMode, Sequence, TICKS_PER_BAR, default_worker_threads};

use ponzik_i18n::tr;

use crate::{Project, SampleLoader};

/// Taille des blocs demandés au moteur.
const BLOCK: usize = 512;
/// Seuil de silence de la queue : −90 dBFS.
const SILENCE: f32 = 3.162_277_7e-5;
/// Silence continu qui termine la queue, en secondes.
const SILENCE_HOLD: f64 = 0.2;
/// Durée maximale de la queue (réverbs, délais), en secondes.
pub const MAX_TAIL: f64 = 10.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderMode {
    /// Toute la playlist, puis la queue jusqu'au silence.
    Song,
    /// Un pattern joué en boucle pendant `bars` mesures, puis la queue.
    Pattern(usize, u32),
}

/// Nombre d'images (échantillons par canal) que dure `ticks` au tempo du projet,
/// arrondi comme le moteur coupe ses blocs en fin de boucle.
pub fn ticks_to_frames(ticks: u32, bpm: f32, sample_rate: u32) -> usize {
    let bpm = (bpm as f64).clamp(20.0, 999.0);
    let spt = sample_rate as f64 * 60.0 / (bpm * PPQ as f64);
    ((ticks as f64 - 1e-9) * spt).ceil().max(0.0) as usize
}

/// Durée jouée (sans la queue), en ticks, et mode de lecture correspondant.
fn span(project: &Project, mode: RenderMode) -> (u32, PlayMode) {
    match mode {
        RenderMode::Song => (project.song_len(), PlayMode::Song),
        RenderMode::Pattern(p, bars) => {
            (bars.max(1) * TICKS_PER_BAR, PlayMode::Pattern(p.min(project.patterns.len().saturating_sub(1))))
        }
    }
}

/// Rendu stéréo entrelacé. La queue après la fin s'arrête au premier passage de
/// [`SILENCE_HOLD`] secondes sous −90 dBFS (silence final retiré), au plus [`MAX_TAIL`].
/// Le moteur calcule sur [`default_worker_threads`] threads en plus.
pub fn render_song(project: &Project, sample_rate: u32, loader: SampleLoader, mode: RenderMode) -> Vec<f32> {
    render_song_threads(project, sample_rate, loader, mode, default_worker_threads())
}

/// Comme [`render_song`], avec `workers` threads de calcul en plus (0 : un
/// seul) ; le résultat est identique au bit près quel que soit leur nombre.
pub fn render_song_threads(
    project: &Project,
    sample_rate: u32,
    loader: SampleLoader,
    mode: RenderMode,
    workers: usize,
) -> Vec<f32> {
    let sample_rate = sample_rate.max(1);
    let (ticks, play_mode) = span(project, mode);
    let mut project = project.clone();
    project.mode = play_mode;
    // L'export ignore la boucle de travail.
    project.loop_range = None;
    // La séquence initiale du moteur est déjà complète (clips audio compris).
    let (mut engine, mut handle) = project.build_engine(sample_rate as f32, loader);
    engine.set_worker_threads(workers);
    let _ = handle.send(Command::Play);

    let frames = ticks_to_frames(ticks, project.bpm, sample_rate);
    let max_tail = (MAX_TAIL * sample_rate as f64) as usize;
    let hold = (SILENCE_HOLD * sample_rate as f64) as usize;
    let mut out = Vec::with_capacity(2 * (frames + max_tail.min(sample_rate as usize * 2)));

    let mut block = [0.0f32; 2 * BLOCK];
    if play_mode == PlayMode::Song {
        // Corps : le moteur s'arrête lui-même à la fin du morceau, ce qui tient
        // compte de l'automation du tempo.
        loop {
            let n = engine.process_to_loop_end(&mut block, 2);
            out.extend_from_slice(&block[..2 * n]);
            handle.collect_garbage();
            if n < BLOCK {
                break;
            }
        }
    } else {
        // Corps : des blocs qui tombent pile sur la fin.
        let mut done = 0;
        while done < frames {
            let n = (frames - done).min(BLOCK);
            engine.process(&mut block[..2 * n], 2);
            out.extend_from_slice(&block[..2 * n]);
            done += n;
            handle.collect_garbage();
        }
    }

    // Queue : plus aucune note (pas de pattern, pas de boucle), les voix s'éteignent.
    let silent =
        Sequence { patterns: Vec::new(), clips: Vec::new(), mode: PlayMode::Pattern(0), ..Sequence::default() };
    let _ = handle.send(Command::SetSequence(Box::new(silent)));
    // Latence compensée par le mixer : tout le morceau sort retardé d'autant.
    let latency = handle.latency();
    let body = out.len();
    let mut last_loud = body;
    let mut tail = 0;
    while tail < max_tail && out.len() - last_loud < 2 * hold {
        let n = (max_tail - tail).min(BLOCK);
        engine.process(&mut block[..2 * n], 2);
        for (k, f) in block[..2 * n].chunks_exact(2).enumerate() {
            if f[0].abs() >= SILENCE || f[1].abs() >= SILENCE {
                last_loud = out.len() + 2 * (k + 1);
            }
        }
        out.extend_from_slice(&block[..2 * n]);
        tail += n;
        handle.collect_garbage();
    }
    out.truncate(last_loud);
    // On retire ce retard pour que l'export commence au premier temps.
    out.drain(..(2 * latency).min(out.len()));
    out
}

/// Une piste par piste de mixer utilisée par un canal audible ou (morceau) un
/// clip audio : (piste, rendu). Chaque stem est rendu en coupant les autres
/// canaux et clips audio ; tous ont la même longueur.
pub fn render_stems(
    project: &Project,
    sample_rate: u32,
    loader: SampleLoader,
    mode: RenderMode,
) -> Vec<(usize, Vec<f32>)> {
    let mut tracks: Vec<usize> = project.channels.iter().filter(|c| !c.muted).map(|c| c.track).collect();
    if matches!(mode, RenderMode::Song) {
        tracks.extend(project.audio_clips.iter().map(|c| c.track));
    }
    tracks.sort_unstable();
    tracks.dedup();
    let mut stems: Vec<(usize, Vec<f32>)> = tracks
        .into_iter()
        .map(|track| {
            let mut p = project.clone();
            for c in &mut p.channels {
                c.muted |= c.track != track;
            }
            // Gain nul plutôt que retrait : la longueur du morceau ne change pas.
            for c in p.audio_clips.iter_mut().filter(|c| c.track != track) {
                c.gain = 0.0;
            }
            (track, render_song(&p, sample_rate, loader, mode))
        })
        .collect();
    let len = stems.iter().map(|(_, s)| s.len()).max().unwrap_or(0);
    for (_, s) in &mut stems {
        s.resize(len, 0.0);
    }
    stems
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BitDepth {
    Pcm16,
    Pcm24,
    Float32,
}

impl BitDepth {
    pub fn bits(self) -> u16 {
        match self {
            BitDepth::Pcm16 => 16,
            BitDepth::Pcm24 => 24,
            BitDepth::Float32 => 32,
        }
    }
}

/// Normalisation appliquée au rendu avant l'écriture.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Normalize {
    Off,
    /// Crête échantillon ramenée à ce niveau (dBFS).
    Peak(f32),
    /// Sonie intégrée ramenée à `lufs` ; le gain s'arrête avant que la crête
    /// vraie ne dépasse `ceiling` (dBTP) : pas de limiteur, rien n'est écrêté.
    Loudness { lufs: f32, ceiling: f32 },
}

/// Réglages d'un export WAV.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ExportOptions {
    pub depth: BitDepth,
    pub normalize: Normalize,
    /// Dither TPDF avant la réduction en 16 bits (sans effet en 24 bits et en flottant).
    pub dither: bool,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self { depth: BitDepth::Pcm24, normalize: Normalize::Off, dither: true }
    }
}

/// Niveaux d'un rendu stéréo entrelacé.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Levels {
    /// Crête échantillon, en dBFS.
    pub peak: f32,
    /// Crête vraie (suréchantillonnée ×4), en dBTP.
    pub true_peak: f32,
    /// Sonie intégrée BS.1770, en LUFS (−∞ pour un silence).
    pub integrated: f32,
}

/// Mesure crête, crête vraie et sonie intégrée d'un rendu stéréo entrelacé.
pub fn measure(samples: &[f32], sample_rate: u32) -> Levels {
    let mut meter = LoudnessMeter::new(sample_rate.max(1) as f32, 2);
    let (mut l, mut r) = (Vec::with_capacity(BLOCK), Vec::with_capacity(BLOCK));
    for chunk in samples.chunks(2 * BLOCK) {
        l.clear();
        r.clear();
        for f in chunk.chunks_exact(2) {
            l.push(f[0]);
            r.push(f[1]);
        }
        meter.process_stereo(&l, &r);
    }
    Levels { peak: meter.sample_peak(), true_peak: meter.true_peak(), integrated: meter.integrated() }
}

/// Gain (dB) qui amène `levels` à la normalisation voulue ; 0 pour un silence.
pub fn normalize_gain(levels: Levels, mode: Normalize) -> f32 {
    let gain = match mode {
        Normalize::Off => 0.0,
        Normalize::Peak(dbfs) => dbfs - levels.peak,
        Normalize::Loudness { lufs, ceiling } => (lufs - levels.integrated).min(ceiling - levels.true_peak),
    };
    // Silence (crête à −200 dB, sonie à −∞) : rien à normaliser.
    if gain.is_finite() && levels.peak > -150.0 { gain } else { 0.0 }
}

/// Normalise un rendu stéréo entrelacé sur place ; renvoie le gain appliqué (dB).
pub fn normalize(samples: &mut [f32], sample_rate: u32, mode: Normalize) -> f32 {
    if mode == Normalize::Off {
        return 0.0;
    }
    let gain = normalize_gain(measure(samples, sample_rate), mode);
    if gain != 0.0 {
        let k = 10f32.powf(gain / 20.0);
        samples.iter_mut().for_each(|s| *s *= k);
    }
    gain
}

/// Normalise puis écrit le rendu ; renvoie le gain de normalisation (dB).
pub fn export_wav(
    path: impl AsRef<Path>,
    samples: &mut [f32],
    sample_rate: u32,
    options: ExportOptions,
) -> io::Result<f32> {
    let gain = normalize(samples, sample_rate, options.normalize);
    let mut w = BufWriter::new(File::create(path)?);
    encode_wav_with(&mut w, samples, sample_rate, options.depth, options.dither)?;
    w.flush()?;
    Ok(gain)
}

/// Écrit un WAV stéréo à partir d'échantillons entrelacés dans [-1, 1] (dither
/// TPDF en 16 bits).
pub fn write_wav(path: impl AsRef<Path>, samples: &[f32], sample_rate: u32, depth: BitDepth) -> io::Result<()> {
    let mut w = BufWriter::new(File::create(path)?);
    encode_wav(&mut w, samples, sample_rate, depth)?;
    w.flush()
}

/// Écrit un WAV de `channels` canaux (1 ou 2), sans dither : pour les prises
/// enregistrées (24 bits ou flottant 32 bits, où le dither n'apporte rien).
pub fn write_wav_channels(
    path: impl AsRef<Path>,
    samples: &[f32],
    channels: u16,
    sample_rate: u32,
    depth: BitDepth,
) -> io::Result<()> {
    let mut w = BufWriter::new(File::create(path)?);
    encode_wav_channels(&mut w, samples, channels, sample_rate, depth, false)?;
    w.flush()
}

/// Encode un WAV stéréo, avec dither TPDF en 16 bits.
pub fn encode_wav(w: &mut impl Write, samples: &[f32], sample_rate: u32, depth: BitDepth) -> io::Result<()> {
    encode_wav_with(w, samples, sample_rate, depth, true)
}

/// Encode un WAV stéréo. `dither` : en 16 bits, bruit TPDF de ±1 LSB
/// (déterministe) ajouté avant l'arrondi, qui change la distorsion de
/// quantification en un souffle constant à −96 dB. Le 24 bits est arrondi tel quel.
pub fn encode_wav_with(
    w: &mut impl Write,
    samples: &[f32],
    sample_rate: u32,
    depth: BitDepth,
    dither: bool,
) -> io::Result<()> {
    encode_wav_channels(w, samples, 2, sample_rate, depth, dither)
}

/// Encode un WAV de `channels` canaux entrelacés (voir [`encode_wav_with`]).
/// Les échantillons sont bornés à ±1, flottant compris.
pub fn encode_wav_channels(
    w: &mut impl Write,
    samples: &[f32],
    channels: u16,
    sample_rate: u32,
    depth: BitDepth,
    dither: bool,
) -> io::Result<()> {
    let channels = channels.max(1);
    let bytes = (depth.bits() / 8) as usize;
    let data_len = u32::try_from(samples.len() * bytes)
        .ok()
        .filter(|&n| n <= u32::MAX - 64)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, tr("WAV file larger than 4 GB")))?;
    let pad = data_len & 1;
    let float = depth == BitDepth::Float32;
    // Le flottant exige un bloc fmt étendu (cbSize) et un bloc fact.
    let fmt_len: u32 = if float { 18 } else { 16 };
    let fact_len: u32 = if float { 12 } else { 0 };
    let riff_len = 4 + (8 + fmt_len) + fact_len + 8 + data_len + pad;
    let block_align = channels * depth.bits() / 8;

    w.write_all(b"RIFF")?;
    w.write_all(&riff_len.to_le_bytes())?;
    w.write_all(b"WAVEfmt ")?;
    w.write_all(&fmt_len.to_le_bytes())?;
    w.write_all(&(if float { 3u16 } else { 1u16 }).to_le_bytes())?;
    w.write_all(&channels.to_le_bytes())?;
    w.write_all(&sample_rate.to_le_bytes())?;
    w.write_all(&(sample_rate * block_align as u32).to_le_bytes())?;
    w.write_all(&block_align.to_le_bytes())?;
    w.write_all(&depth.bits().to_le_bytes())?;
    if float {
        w.write_all(&0u16.to_le_bytes())?;
        w.write_all(b"fact")?;
        w.write_all(&4u32.to_le_bytes())?;
        w.write_all(&((samples.len() / channels as usize) as u32).to_le_bytes())?;
    }
    w.write_all(b"data")?;
    w.write_all(&data_len.to_le_bytes())?;

    let mut noise = WhiteNoise::new(0x5EED_D17E);
    let mut tpdf = || if dither { 0.5 * (noise.next_sample() + noise.next_sample()) } else { 0.0 };
    let mut buf = Vec::with_capacity(BLOCK * 4);
    for chunk in samples.chunks(BLOCK) {
        buf.clear();
        for &s in chunk {
            let s = if s.is_finite() { s.clamp(-1.0, 1.0) } else { 0.0 };
            match depth {
                BitDepth::Pcm16 => {
                    let v = (s * 32_767.0 + tpdf()).round().clamp(-32_768.0, 32_767.0) as i16;
                    buf.extend_from_slice(&v.to_le_bytes());
                }
                BitDepth::Pcm24 => {
                    let v = (s as f64 * 8_388_607.0).round().clamp(-8_388_608.0, 8_388_607.0) as i32;
                    buf.extend_from_slice(&v.to_le_bytes()[..3]);
                }
                BitDepth::Float32 => buf.extend_from_slice(&s.to_le_bytes()),
            }
        }
        w.write_all(&buf)?;
    }
    if pad == 1 {
        w.write_all(&[0])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ponzik_engine::{Note, Sample, TICKS_PER_STEP};

    use super::*;
    use crate::{AudioClipDef, AutoScope, AutoTarget, InstrumentDef, demo_breakcore, demo_project};

    const SR: u32 = 22_050;

    fn no_samples(_: &str) -> Option<Arc<Sample>> {
        None
    }

    fn peak(s: &[f32]) -> f32 {
        s.iter().fold(0.0f32, |m, v| m.max(v.abs()))
    }

    #[test]
    fn song_length_and_level() {
        let p = demo_project();
        let out = render_song(&p, SR, &no_samples, RenderMode::Song);
        assert_eq!(out.len() % 2, 0);
        let frames = ticks_to_frames(p.song_len(), p.bpm, SR);
        // 24 mesures à 124 BPM.
        assert_eq!(frames, (24.0 * 4.0 * 60.0 / 124.0 * SR as f64).ceil() as usize);
        let len = out.len() / 2;
        assert!(len >= frames && len <= frames + (MAX_TAIL * SR as f64) as usize, "{len} / {frames}");
        let pk = peak(&out);
        assert!(pk > 0.05 && pk <= 1.0, "{pk}");
        // La queue finit sous le seuil de silence, sans rejouer le début.
        let tail = &out[2 * frames..];
        assert!(peak(&tail[tail.len().saturating_sub(2 * 64)..]) < 0.01);
        assert!(peak(&out[2 * (len - 1)..]) >= SILENCE || len == frames);
    }

    #[test]
    fn pattern_mode_renders_bars() {
        let p = demo_project();
        let out = render_song(&p, SR, &no_samples, RenderMode::Pattern(0, 2));
        let frames = ticks_to_frames(2 * TICKS_PER_BAR, p.bpm, SR);
        assert!(out.len() / 2 >= frames);
        assert!(peak(&out[..2 * frames]) > 0.05);
        // Pattern absent : borné au dernier, sans paniquer.
        assert!(!render_song(&p, SR, &no_samples, RenderMode::Pattern(99, 1)).is_empty());
    }

    #[test]
    fn export_removes_the_compensated_latency() {
        let mut p = Project::empty();
        let a = p.add_channel("A", InstrumentDef::new("pulse"), 60);
        p.add_note(0, a, Note { start: 0, len: TICKS_PER_STEP * 2, key: 60, vel: 0.5 });
        let onset = |p: &Project| {
            let out = render_song(p, SR, &no_samples, RenderMode::Pattern(0, 1));
            out.iter().position(|s| s.abs() > 1e-4).unwrap()
        };
        let dry = onset(&p);
        let track = p.channels[a].track;
        p.tracks[track].inserts[0] = Some(crate::EffectDef::new("limiter"));
        assert_eq!(onset(&p), dry, "le limiteur ne décale pas l'export");
    }

    #[test]
    fn stems_cover_the_mix() {
        let mut p = Project::empty();
        let a = p.add_channel("A", InstrumentDef::new("pulse"), 60);
        let b = p.add_channel("B", InstrumentDef::new("pulse"), 67);
        let m = p.add_channel("Muet", InstrumentDef::new("pulse"), 72);
        p.channels[m].muted = true;
        p.add_note(0, a, Note { start: 0, len: TICKS_PER_STEP * 2, key: 60, vel: 0.5 });
        p.add_note(0, b, Note { start: TICKS_PER_STEP * 8, len: TICKS_PER_STEP * 2, key: 67, vel: 0.5 });
        p.add_note(0, m, Note { start: 0, len: TICKS_PER_STEP * 2, key: 72, vel: 0.5 });
        let mode = RenderMode::Pattern(0, 1);
        let stems = render_stems(&p, SR, &no_samples, mode);
        assert_eq!(stems.iter().map(|(t, _)| *t).collect::<Vec<_>>(), [p.channels[a].track, p.channels[b].track]);
        assert!(stems.iter().all(|(_, s)| peak(s) > 0.01 && s.len() == stems[0].1.len()));
        // La première moitié n'a que A, la seconde que B.
        let half = 2 * ticks_to_frames(TICKS_PER_BAR / 2, p.bpm, SR);
        assert!(peak(&stems[0].1[half..]) < 0.01);
        assert!(peak(&stems[1].1[..half]) < 1e-6);
        // Mixer linéaire, sans effet : la somme des stems redonne le mix.
        let mix = render_song(&p, SR, &no_samples, mode);
        for (i, &v) in mix.iter().enumerate() {
            let sum: f32 = stems.iter().map(|(_, s)| s.get(i).copied().unwrap_or(0.0)).sum();
            assert!((v - sum).abs() < 1e-4, "{i}: {v} ≠ {sum}");
        }
    }

    #[test]
    fn song_render_includes_audio_clips_and_automation() {
        let mut p = Project::empty();
        p.bpm = 120.0;
        // Un clip audio de deux mesures à la mesure 2, sur la piste 2 dont le gain est automatisé.
        p.audio_clips.push(AudioClipDef {
            path: "dc.wav".into(),
            lane: 0,
            track: 2,
            start: TICKS_PER_BAR,
            len: TICKS_PER_BAR * 2,
            offset: 0.0,
            gain: 0.5,
            fade_in: 0.0,
            fade_out: 0.0,
            take: 0,
            muted: false,
        });
        p.add_automation_point(AutoTarget::TrackGain(2), AutoScope::Song, 0, 1.0);
        p.add_automation_point(AutoTarget::TrackGain(2), AutoScope::Song, TICKS_PER_BAR * 2, 1.0);
        p.add_automation_point(AutoTarget::TrackGain(2), AutoScope::Song, TICKS_PER_BAR * 2 + 1, 0.5);
        assert_eq!(p.song_len(), TICKS_PER_BAR * 3);
        // 4 s à 11 025 Hz, joués à 22 050 Hz.
        let dc = vec![1.0; 44_100];
        let sample = Arc::new(Sample { name: "dc".into(), sample_rate: 11_025.0, left: dc.clone(), right: dc, markers: Vec::new() });
        let loader = |path: &str| (path == "dc.wav").then(|| sample.clone());
        let out = render_song(&p, SR, &loader, RenderMode::Song);
        // Mesure = 2 s à 120 BPM ; le clip commence pile à 2 s et finit avec le morceau.
        let bar = 2 * SR as usize;
        assert_eq!(out.len() / 2, 3 * bar, "pas de queue après un clip audio sec");
        assert_eq!(peak(&out[..2 * bar]), 0.0);
        assert!(out[2 * bar] > 0.0 && out[2 * (3 * bar - 1)] > 0.0);
        assert!((out[2 * (bar + SR as usize / 2)] - 0.5).abs() < 1e-5);
        // Après l'automation : gain de piste 0,5.
        assert!((out[2 * (2 * bar + SR as usize / 2)] - 0.25).abs() < 1e-5);
        // Sans chargeur, le clip est ignoré.
        assert_eq!(peak(&render_song(&p, SR, &no_samples, RenderMode::Song)), 0.0);
    }

    fn u16_at(b: &[u8], i: usize) -> u16 {
        u16::from_le_bytes([b[i], b[i + 1]])
    }

    fn u32_at(b: &[u8], i: usize) -> u32 {
        u32::from_le_bytes(b[i..i + 4].try_into().unwrap())
    }

    #[test]
    fn wav_headers() {
        let samples = [0.0, 1.0, -1.0, 0.5, 2.0, f32::NAN];
        for (depth, bytes) in [(BitDepth::Pcm16, 2usize), (BitDepth::Pcm24, 3), (BitDepth::Float32, 4)] {
            let mut buf = Vec::new();
            encode_wav(&mut buf, &samples, 44_100, depth).unwrap();
            let float = depth == BitDepth::Float32;
            let fmt_len = if float { 18 } else { 16 };
            assert_eq!(&buf[0..4], b"RIFF");
            assert_eq!(u32_at(&buf, 4) as usize, buf.len() - 8, "{depth:?}");
            assert_eq!(&buf[8..16], b"WAVEfmt ");
            assert_eq!(u32_at(&buf, 16), fmt_len);
            assert_eq!(u16_at(&buf, 20), if float { 3 } else { 1 });
            assert_eq!(u16_at(&buf, 22), 2);
            assert_eq!(u32_at(&buf, 24), 44_100);
            assert_eq!(u32_at(&buf, 28), 44_100 * 2 * bytes as u32);
            assert_eq!(u16_at(&buf, 32), 2 * bytes as u16);
            assert_eq!(u16_at(&buf, 34), 8 * bytes as u16);
            let mut at = 20 + fmt_len as usize;
            if float {
                assert_eq!(u16_at(&buf, 36), 0);
                assert_eq!(&buf[at..at + 4], b"fact");
                assert_eq!(u32_at(&buf, at + 8), 3);
                at += 12;
            }
            assert_eq!(&buf[at..at + 4], b"data");
            let data_len = u32_at(&buf, at + 4) as usize;
            assert_eq!(data_len, samples.len() * bytes);
            let data = &buf[at + 8..at + 8 + data_len];
            assert_eq!(buf.len(), at + 8 + data_len + (data_len & 1));
            let values: Vec<f64> = data
                .chunks(bytes)
                .map(|c| match depth {
                    BitDepth::Pcm16 => i16::from_le_bytes([c[0], c[1]]) as f64 / 32_767.0,
                    BitDepth::Pcm24 => (i32::from_le_bytes([0, c[0], c[1], c[2]]) >> 8) as f64 / 8_388_607.0,
                    BitDepth::Float32 => f32::from_le_bytes(c.try_into().unwrap()) as f64,
                })
                .collect();
            let lsb = if float { 0.0 } else { 1.5 / ((1u32 << (8 * bytes - 1)) - 1) as f64 };
            for (v, want) in values.iter().zip([0.0, 1.0, -1.0, 0.5, 1.0, 0.0]) {
                assert!((v - want).abs() <= lsb + 1e-9, "{depth:?} : {v} ≠ {want}");
            }
        }
    }

    #[test]
    fn mono_takes_round_trip_in_24_bits_and_float() {
        let take = [0.0, 0.25, -0.5, 0.999];
        for depth in [BitDepth::Pcm24, BitDepth::Float32] {
            let mut buf = Vec::new();
            encode_wav_channels(&mut buf, &take, 1, 48_000, depth, false).unwrap();
            let s = ponzik_engine::wav::decode_wav(&buf, "prise").unwrap();
            assert_eq!((s.len(), s.sample_rate), (4, 48_000.0));
            for (a, b) in s.left.iter().zip(&take) {
                assert!((a - b).abs() < 1e-6, "{depth:?} : {a} ≠ {b}");
            }
            assert_eq!(s.left, s.right, "le mono se relit sur les deux canaux");
        }
    }

    #[test]
    fn dither_is_triangular_and_small() {
        let mut buf = Vec::new();
        let quiet = vec![0.25 / 32_767.0; 20_000];
        encode_wav(&mut buf, &quiet, 48_000, BitDepth::Pcm16).unwrap();
        let pcm: Vec<i16> = buf[44..].chunks(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
        assert!(pcm.iter().all(|&v| (-1..=2).contains(&v)));
        // Le dither conserve le niveau moyen d'un signal sous le LSB.
        let mean = pcm.iter().map(|&v| v as f64).sum::<f64>() / pcm.len() as f64;
        assert!((mean - 0.25).abs() < 0.03, "{mean}");
    }

    /// Sinus stéréo identique sur les deux canaux, à 997 Hz.
    fn sine(amp: f32, secs: f32, sr: u32) -> Vec<f32> {
        let n = (secs * sr as f32) as usize;
        (0..n)
            .flat_map(|k| {
                let v = amp * (std::f32::consts::TAU * 997.0 * k as f32 / sr as f32).sin();
                [v, v]
            })
            .collect()
    }

    #[test]
    fn peak_normalization_hits_the_target() {
        let mut audio = sine(0.25, 1.0, 48_000);
        let gain = normalize(&mut audio, 48_000, Normalize::Peak(-1.0));
        assert!((gain - (-1.0 - 20.0 * 0.25f32.log10())).abs() < 0.01, "{gain}");
        let pk = peak(&audio);
        assert!((20.0 * pk.log10() + 1.0).abs() < 0.01, "{pk}");
        // Off : rien ne bouge.
        let before = audio.clone();
        assert_eq!(normalize(&mut audio, 48_000, Normalize::Off), 0.0);
        assert_eq!(audio, before);
    }

    #[test]
    fn loudness_normalization_hits_the_target() {
        // Sinus à 1 kHz à −6 dBFS sur les deux canaux : environ −6 LUFS.
        let mut audio = sine(0.5, 5.0, 48_000);
        let before = measure(&audio, 48_000);
        assert!((before.integrated - (-6.0)).abs() < 0.3, "{before:?}");
        normalize(&mut audio, 48_000, Normalize::Loudness { lufs: -14.0, ceiling: -1.0 });
        let after = measure(&audio, 48_000);
        assert!((after.integrated + 14.0).abs() < 0.05, "{after:?}");
        assert!(after.true_peak < -1.0);
    }

    #[test]
    fn loudness_gain_stops_at_the_true_peak_ceiling() {
        // −20 LUFS environ : viser 0 LUFS dépasserait 0 dBTP ; le plafond l'arrête à −1 dBTP.
        let mut audio = sine(0.1, 3.0, 48_000);
        let gain = normalize(&mut audio, 48_000, Normalize::Loudness { lufs: 0.0, ceiling: -1.0 });
        assert!(gain > 15.0 && gain < 20.0, "{gain}");
        let after = measure(&audio, 48_000);
        assert!((after.true_peak + 1.0).abs() < 0.05, "{after:?}");
        assert!(after.integrated < -1.0);
    }

    #[test]
    fn silence_is_left_alone() {
        let mut audio = vec![0.0; 48_000];
        assert_eq!(normalize(&mut audio, 48_000, Normalize::Peak(-0.3)), 0.0);
        assert_eq!(normalize(&mut audio, 48_000, Normalize::Loudness { lufs: -14.0, ceiling: -1.0 }), 0.0);
        assert!(audio.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn dither_only_in_16_bits_and_can_be_turned_off() {
        let quiet = vec![0.25 / 32_767.0; 1_000];
        let pcm16 = |dither| {
            let mut buf = Vec::new();
            encode_wav_with(&mut buf, &quiet, 48_000, BitDepth::Pcm16, dither).unwrap();
            buf[44..].chunks(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect::<Vec<_>>()
        };
        // Sans dither, un signal sous le demi-LSB disparaît ; avec, il reste en moyenne.
        assert!(pcm16(false).iter().all(|&v| v == 0));
        assert!(pcm16(true).iter().any(|&v| v != 0));
        // 24 bits : arrondi exact, sans bruit.
        let mut buf = Vec::new();
        encode_wav(&mut buf, &[0.5, -0.25], 48_000, BitDepth::Pcm24).unwrap();
        let v: Vec<i32> = buf[44..].chunks(3).map(|c| i32::from_le_bytes([0, c[0], c[1], c[2]]) >> 8).collect();
        assert_eq!(v, [(0.5f64 * 8_388_607.0).round() as i32, (-0.25f64 * 8_388_607.0).round() as i32]);
    }

    #[test]
    fn export_normalizes_then_writes() {
        let path = std::env::temp_dir().join(format!("ponzik-export-{}.wav", std::process::id()));
        let mut audio = sine(0.5, 0.5, 48_000);
        let options = ExportOptions { depth: BitDepth::Pcm16, normalize: Normalize::Peak(0.0), dither: true };
        let gain = export_wav(&path, &mut audio, 48_000, options).unwrap();
        assert!((gain - 6.02).abs() < 0.05, "{gain}");
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes.len(), 44 + audio.len() * 2);
        let max = bytes[44..].chunks(2).map(|c| i16::from_le_bytes([c[0], c[1]]).unsigned_abs()).max().unwrap();
        assert!(max >= 32_700, "{max}");
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn writes_file() {
        let path = std::env::temp_dir().join(format!("ponzik-render-{}.wav", std::process::id()));
        write_wav(&path, &[0.0; 8], 48_000, BitDepth::Pcm24).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 44 + 24);
        std::fs::remove_file(&path).unwrap();
    }

    /// `blocks` blocs de tailles variées avec `workers` threads de calcul ; l'ordre
    /// de routage change en route (un ordre périmé sur deux).
    fn render_blocks(p: &Project, workers: usize, blocks: usize) -> Vec<f32> {
        let (mut engine, mut handle) = p.build_engine(48_000.0, &no_samples);
        engine.set_worker_threads(workers);
        assert_eq!(handle.worker_threads(), workers);
        handle.send(Command::Play).unwrap();
        let order = p.routing_order().unwrap();
        let mut buf = [0.0f32; 2 * 600];
        let mut out = Vec::new();
        for b in 0..blocks {
            if b % 3 == 0 {
                let o = if b % 6 == 0 { order.iter().rev().copied().collect() } else { order.clone() };
                handle.send(Command::SetRoutingOrder(o)).unwrap();
            }
            let n = [600, 64, 1, 256, 333][b % 5];
            engine.process(&mut buf[..2 * n], 2);
            out.extend_from_slice(&buf[..2 * n]);
            handle.collect_garbage();
        }
        out
    }

    #[test]
    fn demos_render_bit_identical_on_one_or_several_threads() {
        for p in [demo_project(), demo_breakcore()] {
            let single = render_blocks(&p, 0, 400);
            assert!(peak(&single) > 0.05, "{}", p.name);
            for workers in [1, 3] {
                let multi = render_blocks(&p, workers, 400);
                assert_eq!(single.len(), multi.len());
                let diff = single.iter().zip(&multi).position(|(a, b)| a.to_bits() != b.to_bits());
                assert_eq!(diff, None, "{} : {workers} threads", p.name);
            }
        }
    }

    /// `cargo test --release -p ponzik-project bench_threads -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn bench_threads() {
        let p = demo_breakcore();
        for workers in [0, 1, 2, 3, 5, 7, default_worker_threads()] {
            let started = std::time::Instant::now();
            let out = render_song_threads(&p, 48_000, &no_samples, RenderMode::Song, workers);
            let took = started.elapsed().as_secs_f64();
            let secs = out.len() as f64 / 2.0 / 48_000.0;
            println!("{workers:>2} workers : {took:.3} s for {secs:.1} s of audio ({:.0}x real time)", secs / took);
        }
    }
}
