//! Écriture WAV PCM 16 bits, sans dépendance.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;

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

#[cfg(test)]
mod tests {
    use super::*;

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
