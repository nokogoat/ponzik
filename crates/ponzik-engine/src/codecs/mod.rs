//! Compressed audio decoders, without dependencies: MP3 ([`mp3`]) and Ogg
//! Vorbis ([`ogg`], [`vorbis`]); Ogg Opus is recognized and refused cleanly.
//!
//! [`decode_sample`] is the single entry point of every sample loader (WAV and
//! FLAC included): the format is told by the content first, by the extension
//! otherwise. Decoding runs on the caller's thread, never the audio one.

pub mod mp3;
mod mp3_tables;
pub mod ogg;
pub mod vorbis;

use crate::sample::Sample;
use ponzik_i18n::tr;

/// Decoded audio: the first two channels (a mono source has `right == left`).
#[derive(Clone, Debug, Default)]
pub struct Decoded {
    pub sample_rate: u32,
    /// Channels of the source.
    pub channels: usize,
    pub left: Vec<f32>,
    pub right: Vec<f32>,
}

impl Decoded {
    pub fn frames(&self) -> usize {
        self.left.len()
    }

    pub fn into_sample(self, name: &str) -> Sample {
        Sample { name: name.to_string(), sample_rate: self.sample_rate as f32, left: self.left, right: self.right, markers: Vec::new() }
    }
}

/// Audio file formats Ponzik reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Wav,
    Flac,
    Mp3,
    Ogg,
}

impl Format {
    /// By extension (case-insensitive, without the dot).
    pub fn from_extension(ext: &str) -> Option<Format> {
        match ext.to_ascii_lowercase().as_str() {
            "wav" | "wave" => Some(Format::Wav),
            "flac" => Some(Format::Flac),
            "mp3" => Some(Format::Mp3),
            "ogg" | "oga" | "opus" => Some(Format::Ogg),
            _ => None,
        }
    }

    /// By content (magic bytes), whatever the extension says.
    pub fn sniff(bytes: &[u8]) -> Option<Format> {
        if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WAVE" {
            Some(Format::Wav)
        } else if crate::flac::is_flac(bytes) {
            Some(Format::Flac)
        } else if bytes.starts_with(b"OggS") {
            Some(Format::Ogg)
        } else if mp3::is_mp3(bytes) {
            Some(Format::Mp3)
        } else {
            None
        }
    }
}

/// Extensions of the files the browser lists and the loaders accept (Opus is
/// left out until it is decoded).
pub fn is_supported_extension(ext: &str) -> bool {
    matches!(ext.to_ascii_lowercase().as_str(), "wav" | "wave" | "flac" | "mp3" | "ogg" | "oga")
}

/// Decodes a WAV, FLAC, MP3 or Ogg Vorbis file into a sample named `name`;
/// `ext` (the file extension, may be empty) only matters when the content
/// does not tell.
pub fn decode_sample(bytes: &[u8], name: &str, ext: &str) -> Result<Sample, String> {
    match Format::sniff(bytes).or_else(|| Format::from_extension(ext)) {
        Some(Format::Wav) => crate::wav::decode_wav(bytes, name),
        Some(Format::Flac) => crate::flac::decode_sample(bytes, name),
        Some(Format::Mp3) => Ok(mp3::decode(bytes)?.into_sample(name)),
        Some(Format::Ogg) => Ok(ogg::decode(bytes)?.into_sample(name)),
        None => Err(tr("Unsupported audio format").into()),
    }
}

#[cfg(test)]
pub(crate) mod testing {
    /// SNR in dB of `got` against `amp · sin(2π · freq · i / rate)`.
    pub fn sine_snr(got: &[f32], amp: f64, freq: f64, rate: f64) -> f64 {
        let (mut signal, mut noise) = (0.0, 0.0);
        for (i, &v) in got.iter().enumerate() {
            let want = amp * (std::f64::consts::TAU * freq * i as f64 / rate).sin();
            signal += want * want;
            noise += (want - v as f64).powi(2);
        }
        10.0 * (signal / noise.max(1e-30)).log10()
    }

    /// Feeds `decode` truncated, corrupted and random versions of `file`: it
    /// may fail, never panic (the test fails if it does).
    pub fn never_panics(file: &[u8], decode: impl Fn(&[u8])) {
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for k in 0..64 {
            decode(&file[..file.len() * k / 64]);
        }
        for _ in 0..300 {
            let mut bad = file.to_vec();
            for _ in 0..1 + next() % 8 {
                let at = (next() % bad.len() as u64) as usize;
                bad[at] = next() as u8;
            }
            decode(&bad);
        }
        for _ in 0..50 {
            let mut bad = file[..file.len().min(64)].to_vec();
            bad.extend((0..next() % 20_000).map(|_| next() as u8));
            decode(&bad);
        }
    }
}
