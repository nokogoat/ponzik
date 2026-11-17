//! Ogg container (RFC 3533): pages checked by their CRC, packets put back
//! together across pages, and the decoding of the Vorbis stream inside,
//! trimmed to its granule positions. Chained streams with the same format
//! follow each other; Opus streams are recognized and refused cleanly.

use super::Decoded;
use super::vorbis::{Identification, Vorbis};
use ponzik_i18n::tr;

fn corrupt() -> String {
    tr("Corrupt Ogg file").into()
}

/// CRC-32 of the Ogg pages: polynomial 0x04C11DB7, not reflected, zero start.
pub fn crc32(data: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        std::array::from_fn(|i| {
            let mut r = (i as u32) << 24;
            for _ in 0..8 {
                r = if r & 0x8000_0000 != 0 { r << 1 ^ 0x04C1_1DB7 } else { r << 1 };
            }
            r
        })
    });
    data.iter().fold(0u32, |crc, &b| crc << 8 ^ table[((crc >> 24) as u8 ^ b) as usize])
}

const CONTINUED: u8 = 1;
const FIRST: u8 = 2;
const LAST: u8 = 4;

struct Page<'a> {
    flags: u8,
    granule: i64,
    serial: u32,
    sequence: u32,
    lacing: &'a [u8],
    body: &'a [u8],
}

/// The page at `at` and its length, if it is whole and its CRC is right.
fn page_at(bytes: &[u8], at: usize) -> Option<(Page<'_>, usize)> {
    let h = bytes.get(at..at + 27)?;
    if &h[..4] != b"OggS" || h[4] != 0 {
        return None;
    }
    let segments = h[26] as usize;
    let lacing = bytes.get(at + 27..at + 27 + segments)?;
    let body_len: usize = lacing.iter().map(|&l| l as usize).sum();
    let len = 27 + segments + body_len;
    let whole = bytes.get(at..at + len)?;
    let stored = u32::from_le_bytes([h[22], h[23], h[24], h[25]]);
    let mut copy = whole.to_vec();
    copy[22..26].fill(0);
    if crc32(&copy) != stored {
        return None;
    }
    let page = Page {
        flags: h[5],
        granule: i64::from_le_bytes(h[6..14].try_into().ok()?),
        serial: u32::from_le_bytes([h[14], h[15], h[16], h[17]]),
        sequence: u32::from_le_bytes([h[18], h[19], h[20], h[21]]),
        lacing,
        body: &whole[27 + segments..],
    };
    Some((page, len))
}

/// A packet of one logical stream.
pub struct Packet {
    pub serial: u32,
    pub data: Vec<u8>,
    /// Granule position of the page, on the last packet finished there.
    pub granule: Option<i64>,
    /// First packet of its stream.
    pub first: bool,
    /// Last packet of its stream.
    pub last: bool,
}

/// Every packet of the file, in order; damaged pages are skipped (with the
/// packets they were part of) and garbage between pages is passed over.
pub fn packets(bytes: &[u8]) -> Vec<Packet> {
    struct Partial {
        serial: u32,
        data: Vec<u8>,
        next_sequence: u32,
        /// The last page ended inside a packet.
        open: bool,
        seen_packet: bool,
    }
    let mut partials: Vec<Partial> = Vec::new();
    let mut out = Vec::new();
    let mut at = 0;
    while at + 27 <= bytes.len() {
        let Some((page, len)) = page_at(bytes, at) else {
            // Resynchronize on the next capture pattern.
            at += 1 + bytes[at + 1..].windows(4).position(|w| w == b"OggS").unwrap_or(bytes.len());
            continue;
        };
        at += len;
        let index = partials.iter().position(|p| p.serial == page.serial).unwrap_or_else(|| {
            partials.push(Partial { serial: page.serial, data: Vec::new(), next_sequence: page.sequence, open: false, seen_packet: false });
            partials.len() - 1
        });
        let state = &mut partials[index];
        let lost = page.sequence != state.next_sequence;
        state.next_sequence = page.sequence.wrapping_add(1);
        let continued = page.flags & CONTINUED != 0;
        // The end of a packet whose start was lost is dropped.
        let mut dropping = continued && (lost || !state.open);
        if !continued || dropping {
            state.data.clear();
        }
        let finished = page.lacing.iter().filter(|&&l| l < 255).count();
        let mut done = 0;
        let mut offset = 0;
        for &l in page.lacing {
            let chunk = &page.body[offset..offset + l as usize];
            offset += l as usize;
            if !dropping {
                state.data.extend_from_slice(chunk);
            }
            if l < 255 {
                done += 1;
                if !dropping {
                    out.push(Packet {
                        serial: page.serial,
                        data: std::mem::take(&mut state.data),
                        granule: (done == finished && page.granule != -1).then_some(page.granule),
                        first: page.flags & FIRST != 0 && !state.seen_packet,
                        last: page.flags & LAST != 0 && done == finished,
                    });
                    state.seen_packet = true;
                }
                dropping = false;
            }
        }
        state.open = !dropping && page.lacing.last() == Some(&255);
    }
    out
}

/// One Vorbis stream of the file.
struct Stream {
    serial: u32,
    headers: Vec<Vec<u8>>,
    decoder: Option<Vorbis>,
    /// Samples of this stream output so far.
    produced: i64,
    /// Granule position of its first sample, once known.
    origin: Option<i64>,
    /// Where its samples start in the output.
    start: usize,
    ended: bool,
}

/// The two channels Ponzik keeps, in the Vorbis channel order.
fn stereo_pair(channels: usize) -> (usize, usize) {
    match channels {
        1 => (0, 0),
        3 | 5..=8 => (0, 2),
        _ => (0, 1),
    }
}

/// Decodes the (first) Vorbis stream of an Ogg file, followed by the chained
/// streams with the same rate and channel count.
pub fn decode(bytes: &[u8]) -> Result<Decoded, String> {
    let mut opus = false;
    let mut format: Option<(u32, usize)> = None;
    let mut stream: Option<Stream> = None;
    let mut channels: Vec<Vec<f32>> = Vec::new();
    for p in packets(bytes) {
        if p.first {
            opus |= p.data.starts_with(b"OpusHead");
            if p.data.starts_with(b"\x01vorbis") && stream.as_ref().is_none_or(|s| s.ended) {
                let id = Identification::parse(&p.data).map_err(|_| corrupt())?;
                if format.is_some_and(|f| f != (id.sample_rate, id.channels)) {
                    break;
                }
                if format.is_none() {
                    format = Some((id.sample_rate, id.channels));
                    channels = vec![Vec::new(); id.channels];
                }
                let start = channels[0].len();
                stream = Some(Stream { serial: p.serial, headers: vec![p.data], decoder: None, produced: 0, origin: None, start, ended: false });
            }
            continue;
        }
        let Some(s) = stream.as_mut().filter(|s| s.serial == p.serial && !s.ended) else { continue };
        s.ended = p.last;
        let Some(decoder) = s.decoder.as_mut() else {
            s.headers.push(p.data);
            if s.headers.len() == 3 {
                if !s.headers[1].starts_with(b"\x03vorbis") {
                    return Err(corrupt());
                }
                s.decoder = Some(Vorbis::new(&s.headers[0], &s.headers[2]).map_err(|_| corrupt())?);
            }
            continue;
        };
        s.produced += decoder.decode(&p.data, &mut channels) as i64;
        let Some(granule) = p.granule else { continue };
        let start = s.start;
        if s.origin.is_none() && p.last {
            // A single page: its granule is only the end.
            s.origin = Some(0);
        }
        if s.origin.is_none() {
            // Samples before granule 0 are encoder priming: cropped.
            let origin = granule - s.produced;
            if origin < 0 {
                let drop = (-origin).min(s.produced) as usize;
                for c in channels.iter_mut() {
                    c.drain(start..start + drop);
                }
                s.produced -= drop as i64;
            }
            s.origin = Some(origin.max(0));
        }
        if p.last {
            // The last page tells the exact length.
            let keep = (granule - s.origin.unwrap_or(0)).clamp(0, s.produced);
            for c in channels.iter_mut() {
                c.truncate(start + keep as usize);
            }
        }
    }
    let Some((sample_rate, count)) = format else {
        return Err(if opus { tr("Opus files are not supported yet").into() } else { corrupt() });
    };
    if stream.is_none_or(|s| s.decoder.is_none()) {
        return Err(corrupt());
    }
    let (l, r) = stereo_pair(count);
    let right = if r == l { channels[l].clone() } else { std::mem::take(&mut channels[r]) };
    let left = std::mem::take(&mut channels[l]);
    Ok(Decoded { sample_rate, channels: count, left, right })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_of_known_data() {
        assert_eq!(crc32(b""), 0);
        // CRC-32/MPEG-2 without the final inversion and with zero start:
        // "123456789" gives 0x89A1897F.
        assert_eq!(crc32(b"123456789"), 0x89A1_897F);
    }

    /// One page holding `packets` (each under 255 bytes).
    fn page(serial: u32, sequence: u32, flags: u8, granule: i64, packets: &[&[u8]]) -> Vec<u8> {
        let mut p = b"OggS\0".to_vec();
        p.push(flags);
        p.extend_from_slice(&granule.to_le_bytes());
        p.extend_from_slice(&serial.to_le_bytes());
        p.extend_from_slice(&sequence.to_le_bytes());
        p.extend_from_slice(&[0; 4]);
        p.push(packets.len() as u8);
        p.extend(packets.iter().map(|d| d.len() as u8));
        for d in packets {
            p.extend_from_slice(d);
        }
        let crc = crc32(&p);
        p[22..26].copy_from_slice(&crc.to_le_bytes());
        p
    }

    #[test]
    fn packets_cross_pages_and_bad_pages_are_dropped() {
        let big = vec![7u8; 300];
        let mut file = page(1, 0, FIRST, 0, &[b"head"]);
        // A 300-byte packet over two pages: 255 + 45.
        let mut p = b"OggS\0\0".to_vec();
        p.extend_from_slice(&(-1i64).to_le_bytes());
        p.extend_from_slice(&1u32.to_le_bytes());
        p.extend_from_slice(&1u32.to_le_bytes());
        p.extend_from_slice(&[0; 4]);
        p.extend_from_slice(&[1, 255]);
        p.extend_from_slice(&big[..255]);
        let crc = crc32(&p);
        p[22..26].copy_from_slice(&crc.to_le_bytes());
        file.extend_from_slice(&p);
        file.extend_from_slice(b"garbage");
        file.extend_from_slice(&page(1, 2, CONTINUED, 1_000, &[&big[255..], b"x"]));
        let mut damaged = page(1, 3, 0, 2_000, &[b"lost"]);
        damaged[30] ^= 1;
        file.extend_from_slice(&damaged);
        file.extend_from_slice(&page(1, 4, LAST, 3_000, &[b"end"]));
        let got = packets(&file);
        let data: Vec<&[u8]> = got.iter().map(|p| p.data.as_slice()).collect();
        assert_eq!(data, vec![&b"head"[..], &big[..], b"x", b"end"]);
        assert!(got[0].first && got[3].last);
        assert_eq!(got.iter().map(|p| p.granule).collect::<Vec<_>>(), vec![Some(0), None, Some(1_000), Some(3_000)]);
    }

    /// libvorbis, quality 0, 32 kHz: 16 000 frames of a 440 Hz sine at 0.5
    /// on the left and a 660 Hz one at 0.3 on the right.
    const LIBVORBIS: &[u8] = include_bytes!("../../tests/data/libvorbis-sine-32k-stereo.ogg");

    #[test]
    fn libvorbis_file_decodes_to_its_sines() {
        use crate::codecs::testing::sine_snr;
        let d = decode(LIBVORBIS).unwrap();
        assert_eq!((d.sample_rate, d.channels, d.frames()), (32_000, 2, 16_000), "trimmed to the last granule");
        assert!(sine_snr(&d.left, 0.5, 440.0, 32_000.0) > 20.0);
        assert!(sine_snr(&d.right, 0.3, 660.0, 32_000.0) > 20.0);
    }

    #[test]
    fn damaged_files_never_panic() {
        crate::codecs::testing::never_panics(LIBVORBIS, |b| {
            let _ = decode(b);
        });
        // Damaged audio pages (CRC fixed up) reach the Vorbis decoder itself.
        let pages: Vec<usize> = LIBVORBIS.windows(4).enumerate().filter(|(_, w)| w == b"OggS").map(|(i, _)| i).collect();
        let audio = *pages.last().unwrap();
        let mut seed = 0x1234_5678_9ABC_DEF1u64;
        for _ in 0..300 {
            let mut bad = LIBVORBIS.to_vec();
            for _ in 0..4 {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let at = pages[1] + 28 + (seed as usize % (bad.len() - pages[1] - 28));
                bad[at] = (seed >> 32) as u8;
            }
            for &start in &pages {
                let Some(len) = page_len(&bad, start) else { continue };
                bad[start + 22..start + 26].fill(0);
                let crc = crc32(&bad[start..start + len]);
                bad[start + 22..start + 26].copy_from_slice(&crc.to_le_bytes());
            }
            let _ = decode(&bad);
        }
        assert!(audio > 0);
    }

    /// Length of the page at `at`, from its header alone.
    fn page_len(bytes: &[u8], at: usize) -> Option<usize> {
        let segments = *bytes.get(at + 26)? as usize;
        let lacing = bytes.get(at + 27..at + 27 + segments)?;
        let len = 27 + segments + lacing.iter().map(|&l| l as usize).sum::<usize>();
        (at + len <= bytes.len()).then_some(len)
    }

    #[test]
    fn opus_is_refused_cleanly() {
        let mut head = b"OpusHead".to_vec();
        head.extend_from_slice(&[1, 2, 0x38, 1, 0x80, 0xBB, 0, 0, 0, 0, 0]);
        let mut file = page(9, 0, FIRST, 0, &[&head]);
        file.extend_from_slice(&page(9, 1, 0, 0, &[b"OpusTags\0\0\0\0\0\0\0\0"]));
        file.extend_from_slice(&page(9, 2, LAST, 960, &[&[0xFC, 0xFF, 0xFE]]));
        let e = decode(&file).unwrap_err();
        assert!(e.contains("Opus"), "{e}");
        assert!(decode(b"OggS").is_err());
        assert!(decode(b"").is_err());
    }
}
