//! Lecture et écriture WAV, sans dépendance.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;

use crate::sample::Sample;

/// `samples` entrelacés, dans [-1, 1].
pub fn write_wav_i16(path: impl AsRef<Path>, sample_rate: u32, channels: u16, samples: &[f32]) -> io::Result<()> {
    let mut w = BufWriter::new(File::create(path)?);
    encode_wav_i16(&mut w, sample_rate, channels, samples)?;
    w.flush()
}

pub fn encode_wav_i16(w: &mut impl Write, sample_rate: u32, channels: u16, samples: &[f32]) -> io::Result<()> {
    let data_len = u32::try_from(samples.len() * 2)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "fichier WAV > 4 Go"))?;
    let block_align = channels * 2;
    w.write_all(b"RIFF")?;
    w.write_all(&(36 + data_len).to_le_bytes())?;
    w.write_all(b"WAVEfmt ")?;
    w.write_all(&16u32.to_le_bytes())?;
    w.write_all(&1u16.to_le_bytes())?; // PCM
    w.write_all(&channels.to_le_bytes())?;
    w.write_all(&sample_rate.to_le_bytes())?;
    w.write_all(&(sample_rate * block_align as u32).to_le_bytes())?;
    w.write_all(&block_align.to_le_bytes())?;
    w.write_all(&16u16.to_le_bytes())?;
    w.write_all(b"data")?;
    w.write_all(&data_len.to_le_bytes())?;
    for &s in samples {
        let v = (s.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16;
        w.write_all(&v.to_le_bytes())?;
    }
    Ok(())
}

/// Écrit un échantillon en WAV PCM 24 bits (mono si les deux canaux sont
/// identiques), avec ses marqueurs dans un bloc `cue `.
pub fn write_sample(path: impl AsRef<Path>, sample: &Sample) -> io::Result<()> {
    let mut w = BufWriter::new(File::create(path)?);
    encode_sample(&mut w, sample)?;
    w.flush()
}

pub fn encode_sample(w: &mut impl Write, sample: &Sample) -> io::Result<()> {
    let channels: u16 = if sample.is_mono() { 1 } else { 2 };
    let frames = sample.len();
    let data_len = u32::try_from(frames * channels as usize * 3)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "fichier WAV > 4 Go"))?;
    let markers: Vec<u32> = sample.markers.iter().filter(|&&m| m < frames).map(|&m| m as u32).collect();
    let cue_len = if markers.is_empty() { 0 } else { 4 + 24 * markers.len() as u32 };
    let cue_chunk = if markers.is_empty() { 0 } else { 8 + cue_len };
    let pad = data_len & 1;
    let rate = sample.sample_rate.round().max(1.0) as u32;
    let block_align = channels * 3;
    w.write_all(b"RIFF")?;
    w.write_all(&(36 + data_len + pad + cue_chunk).to_le_bytes())?;
    w.write_all(b"WAVEfmt ")?;
    w.write_all(&16u32.to_le_bytes())?;
    w.write_all(&1u16.to_le_bytes())?; // PCM
    w.write_all(&channels.to_le_bytes())?;
    w.write_all(&rate.to_le_bytes())?;
    w.write_all(&(rate * block_align as u32).to_le_bytes())?;
    w.write_all(&block_align.to_le_bytes())?;
    w.write_all(&24u16.to_le_bytes())?;
    w.write_all(b"data")?;
    w.write_all(&data_len.to_le_bytes())?;
    let put = |w: &mut dyn Write, s: f32| {
        let v = (s.clamp(-1.0, 1.0) * 8_388_607.0).round() as i32;
        w.write_all(&v.to_le_bytes()[..3])
    };
    for i in 0..frames {
        put(w, sample.left[i])?;
        if channels == 2 {
            put(w, sample.right[i])?;
        }
    }
    if pad == 1 {
        w.write_all(&[0])?;
    }
    if !markers.is_empty() {
        w.write_all(b"cue ")?;
        w.write_all(&cue_len.to_le_bytes())?;
        w.write_all(&(markers.len() as u32).to_le_bytes())?;
        for (k, &m) in markers.iter().enumerate() {
            // Identifiant, position, bloc « data », début de bloc, début de paquet, image.
            w.write_all(&(k as u32 + 1).to_le_bytes())?;
            w.write_all(&m.to_le_bytes())?;
            w.write_all(b"data")?;
            w.write_all(&0u32.to_le_bytes())?;
            w.write_all(&0u32.to_le_bytes())?;
            w.write_all(&m.to_le_bytes())?;
        }
    }
    Ok(())
}

/// Charge un fichier WAV ; l'échantillon prend le nom du fichier sans extension.
pub fn load_wav(path: impl AsRef<Path>) -> Result<Sample, String> {
    let path = path.as_ref();
    let bytes = std::fs::read(path).map_err(|e| format!("{} : {e}", path.display()))?;
    let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    decode_wav(&bytes, &name)
}

/// Décode un WAV : PCM entier 8/16/24/32 bits, flottant 32/64 bits, y compris
/// WAVE_FORMAT_EXTENSIBLE. Au-delà de deux canaux, seuls les deux premiers sont gardés.
pub fn decode_wav(bytes: &[u8], name: &str) -> Result<Sample, String> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("ce n'est pas un fichier WAV".into());
    }
    let mut format = None;
    let mut data = None;
    let mut cues = Vec::new();
    let mut pos = 12;
    while pos + 8 <= bytes.len() {
        let size = u32_at(bytes, pos + 4) as usize;
        let start = pos + 8;
        let body = &bytes[start..start.saturating_add(size).min(bytes.len())];
        match &bytes[pos..pos + 4] {
            b"fmt " => format = Some(Format::parse(body)?),
            b"data" => data = Some(body),
            b"cue " if body.len() >= 4 => {
                // Points de repère : 24 octets chacun, l'image est le dernier champ.
                let n = u32_at(body, 0) as usize;
                cues = (0..n)
                    .map(|k| 4 + 24 * k)
                    .take_while(|o| o + 24 <= body.len())
                    .map(|o| u32_at(body, o + 20) as usize)
                    .collect();
            }
            _ => {}
        }
        // Un bloc de taille impaire est suivi d'un octet de bourrage.
        pos = start.saturating_add(size).saturating_add(size & 1);
    }
    let f = format.ok_or("bloc « fmt » manquant")?;
    let data = data.ok_or("bloc « data » manquant")?;

    let width = f.bits as usize / 8;
    let frame = width * f.channels;
    let frames = data.len() / frame;
    let read = |o: usize| -> f32 {
        let b = &data[o..o + width];
        let v = match (f.float, f.bits) {
            (false, 8) => (b[0] as f32 - 128.0) / 128.0,
            (false, 16) => i16::from_le_bytes([b[0], b[1]]) as f32 / 32_768.0,
            (false, 24) => (i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8) as f32 / 8_388_608.0,
            (false, _) => i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2_147_483_648.0,
            (true, 32) => f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            (true, _) => f64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]) as f32,
        };
        if v.is_finite() { v } else { 0.0 }
    };
    let mut left = Vec::with_capacity(frames);
    let mut right = Vec::with_capacity(frames);
    for i in 0..frames {
        let o = i * frame;
        let l = read(o);
        left.push(l);
        right.push(if f.channels > 1 { read(o + width) } else { l });
    }
    cues.retain(|&m| m < frames);
    cues.sort_unstable();
    cues.dedup();
    Ok(Sample { name: name.to_string(), sample_rate: f.sample_rate as f32, left, right, markers: cues })
}

struct Format {
    float: bool,
    channels: usize,
    sample_rate: u32,
    bits: u16,
}

impl Format {
    fn parse(b: &[u8]) -> Result<Self, String> {
        if b.len() < 16 {
            return Err("bloc « fmt » trop court".into());
        }
        let mut tag = u16_at(b, 0);
        if tag == 0xFFFE {
            // WAVE_FORMAT_EXTENSIBLE : le vrai format ouvre le GUID SubFormat.
            if b.len() < 26 {
                return Err("bloc « fmt » étendu trop court".into());
            }
            tag = u16_at(b, 24);
        }
        let float = match tag {
            1 => false,
            3 => true,
            t => return Err(format!("format WAV non géré ({t:#06x})")),
        };
        let bits = u16_at(b, 14);
        if !matches!((float, bits), (false, 8 | 16 | 24 | 32) | (true, 32 | 64)) {
            return Err(format!("profondeur non gérée : {bits} bits"));
        }
        let channels = u16_at(b, 2) as usize;
        let sample_rate = u32_at(b, 4);
        if channels == 0 || sample_rate == 0 {
            return Err("en-tête WAV invalide".into());
        }
        Ok(Self { float, channels, sample_rate, bits })
    }
}

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WAV synthétique, avec un bloc inconnu de taille impaire avant les données.
    fn build(tag: u16, channels: u16, bits: u16, extensible: bool, payload: &[u8]) -> Vec<u8> {
        let mut fmt = Vec::new();
        fmt.extend_from_slice(&(if extensible { 0xFFFE } else { tag }).to_le_bytes());
        fmt.extend_from_slice(&channels.to_le_bytes());
        fmt.extend_from_slice(&44_100u32.to_le_bytes());
        let align = channels * bits / 8;
        fmt.extend_from_slice(&(44_100 * align as u32).to_le_bytes());
        fmt.extend_from_slice(&align.to_le_bytes());
        fmt.extend_from_slice(&bits.to_le_bytes());
        if extensible {
            fmt.extend_from_slice(&22u16.to_le_bytes());
            fmt.extend_from_slice(&bits.to_le_bytes());
            fmt.extend_from_slice(&3u32.to_le_bytes());
            fmt.extend_from_slice(&tag.to_le_bytes());
            fmt.extend_from_slice(&[0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0xAA, 0, 0x38, 0x9B, 0x71]);
        }
        let mut out = b"RIFF\0\0\0\0WAVE".to_vec();
        for (id, body) in [(b"fmt ", &fmt[..]), (b"LIST", &b"abc"[..]), (b"data", payload)] {
            out.extend_from_slice(id);
            out.extend_from_slice(&(body.len() as u32).to_le_bytes());
            out.extend_from_slice(body);
            if body.len() % 2 == 1 {
                out.push(0);
            }
        }
        let riff = (out.len() - 8) as u32;
        out[4..8].copy_from_slice(&riff.to_le_bytes());
        out
    }

    #[test]
    fn roundtrip_16_bits_stereo() {
        let mut buf = Vec::new();
        encode_wav_i16(&mut buf, 48_000, 2, &[0.0, 0.5, -0.5, 1.0]).unwrap();
        let s = decode_wav(&buf, "test").unwrap();
        assert_eq!(s.name, "test");
        assert_eq!(s.sample_rate, 48_000.0);
        assert_eq!(s.len(), 2);
        assert!((s.left[1] + 0.5).abs() < 1e-4 && (s.right[0] - 0.5).abs() < 1e-4);
    }

    #[test]
    fn integer_depths() {
        let s = decode_wav(&build(1, 1, 8, false, &[128, 255, 0]), "").unwrap();
        assert_eq!(s.left, [0.0, 127.0 / 128.0, -1.0]);
        assert_eq!(s.right, s.left);
        let s = decode_wav(&build(1, 1, 24, false, &[0xFF, 0xFF, 0x7F, 0, 0, 0x80, 0, 0, 0]), "").unwrap();
        assert!((s.left[0] - 1.0).abs() < 1e-6 && s.left[1] == -1.0 && s.left[2] == 0.0);
        let mut p = Vec::new();
        p.extend_from_slice(&i32::MIN.to_le_bytes());
        p.extend_from_slice(&(1i32 << 30).to_le_bytes());
        let s = decode_wav(&build(1, 1, 32, true, &p), "").unwrap();
        assert_eq!(s.left, [-1.0, 0.5]);
    }

    #[test]
    fn float_and_extra_channels() {
        let mut p = Vec::new();
        for v in [0.25f32, -0.75] {
            p.extend_from_slice(&v.to_le_bytes());
        }
        let s = decode_wav(&build(3, 2, 32, true, &p), "").unwrap();
        assert_eq!((s.left[0], s.right[0]), (0.25, -0.75));

        let mut p = Vec::new();
        for v in [0.1f64, 0.2, 0.3, f64::NAN, 0.5, 0.6] {
            p.extend_from_slice(&v.to_le_bytes());
        }
        let s = decode_wav(&build(3, 3, 64, false, &p), "").unwrap();
        assert_eq!(s.left, [0.1, 0.0]);
        assert_eq!(s.right, [0.2, 0.5]);
    }

    #[test]
    fn rejects_bad_input() {
        assert!(decode_wav(b"pas un wav du tout", "").is_err());
        assert!(decode_wav(&build(1, 1, 12, false, &[0, 0]), "").is_err());
        assert!(decode_wav(&build(2, 1, 16, false, &[0, 0]), "").is_err());
        let mut no_data = build(1, 1, 16, false, &[]);
        no_data.truncate(12 + 8 + 16);
        assert!(decode_wav(&no_data, "").is_err());
    }

    #[test]
    fn loads_from_disk() {
        let path = std::env::temp_dir().join(format!("ponzik-wav-{}.wav", std::process::id()));
        write_wav_i16(&path, 22_050, 1, &[0.0, 0.5]).unwrap();
        let s = load_wav(&path);
        std::fs::remove_file(&path).ok();
        let s = s.unwrap();
        assert_eq!(s.sample_rate, 22_050.0);
        assert!(s.name.starts_with("ponzik-wav-"));
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn sample_roundtrip_keeps_markers() {
        let left: Vec<f32> = (0..1_001).map(|i| (i as f32 * 0.01).sin() * 0.9).collect();
        let right: Vec<f32> = left.iter().map(|v| -v).collect();
        let s = Sample { name: "x".into(), sample_rate: 44_100.0, left, right, markers: vec![0, 250, 700, 5_000] };
        let mut buf = Vec::new();
        encode_sample(&mut buf, &s).unwrap();
        assert_eq!(buf.len() % 2, 0, "taille RIFF paire");
        let back = decode_wav(&buf, "x").unwrap();
        assert_eq!(back.markers, [0, 250, 700]);
        assert_eq!(back.sample_rate, 44_100.0);
        assert_eq!(back.len(), 1_001);
        for (a, b) in s.left.iter().zip(&back.left).chain(s.right.iter().zip(&back.right)) {
            assert!((a - b).abs() < 1e-6, "{a} {b}");
        }
        // Mono : un seul canal écrit, pas de bloc « cue » sans marqueurs.
        let mono = Sample { right: s.left.clone(), markers: Vec::new(), ..s.clone() };
        let mut buf = Vec::new();
        encode_sample(&mut buf, &mono).unwrap();
        assert_eq!(u16::from_le_bytes([buf[22], buf[23]]), 1);
        assert!(!buf.windows(4).any(|w| w == b"cue "));
        let back = decode_wav(&buf, "m").unwrap();
        assert_eq!(back.right, back.left);
    }

    #[test]
    fn header_and_payload() {
        let mut buf = Vec::new();
        encode_wav_i16(&mut buf, 48_000, 2, &[0.0, 1.0, -1.0, 2.0]).unwrap();
        assert_eq!(buf.len(), 44 + 8);
        assert_eq!(&buf[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(buf[4..8].try_into().unwrap()), 44);
        assert_eq!(&buf[8..16], b"WAVEfmt ");
        assert_eq!(u32::from_le_bytes(buf[28..32].try_into().unwrap()), 192_000);
        assert_eq!(&buf[36..40], b"data");
        let pcm: Vec<i16> = buf[44..].chunks(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
        assert_eq!(pcm, [0, 32_767, -32_767, 32_767]);
    }
}
