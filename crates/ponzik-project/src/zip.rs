//! Zip archives (PKWARE APPNOTE), read and written here: compressed MusicXML
//! (`.mxl`) and Studio One songs are zip files.
//!
//! The reader goes through the central directory at the end of the file and
//! reads stored and DEFLATE entries (the two methods every tool writes),
//! checking their CRC-32. The writer makes the same kind of archive (tests
//! build their fixtures with it).

use crate::gzip::{crc32, deflate, inflate};

/// Stored (no compression).
const STORED: u16 = 0;
/// DEFLATE.
const DEFLATED: u16 = 8;

/// One file of an archive.
#[derive(Clone, Debug, PartialEq)]
pub struct ZipEntry {
    /// Path inside the archive, `/`-separated.
    pub name: String,
    method: u16,
    crc: u32,
    size: usize,
    /// Where its compressed bytes are, in the archive.
    data: std::ops::Range<usize>,
}

impl ZipEntry {
    pub fn is_dir(&self) -> bool {
        self.name.ends_with('/')
    }

    /// Size once extracted.
    pub fn size(&self) -> usize {
        self.size
    }
}

fn u16_at(b: &[u8], at: usize) -> Result<u16, String> {
    b.get(at..at + 2).map(|x| u16::from_le_bytes([x[0], x[1]])).ok_or_else(|| "truncated zip archive".into())
}

fn u32_at(b: &[u8], at: usize) -> Result<u32, String> {
    b.get(at..at + 4).map(|x| u32::from_le_bytes([x[0], x[1], x[2], x[3]])).ok_or_else(|| "truncated zip archive".into())
}

/// True if `bytes` start like a zip archive.
pub fn is_zip(bytes: &[u8]) -> bool {
    bytes.starts_with(b"PK\x03\x04") || bytes.starts_with(b"PK\x05\x06")
}

/// The files of a zip archive, in the order of its central directory.
pub fn read_zip(bytes: &[u8]) -> Result<Vec<ZipEntry>, String> {
    // The end of central directory record: 22 bytes and a comment of at most 64 KiB.
    let lowest = bytes.len().saturating_sub(22 + 0xffff);
    let eocd = (lowest..=bytes.len().saturating_sub(22))
        .rev()
        .find(|&i| bytes[i..].starts_with(b"PK\x05\x06"))
        .ok_or("not a zip archive (no central directory)")?;
    let count = u16_at(bytes, eocd + 10)? as usize;
    let mut at = u32_at(bytes, eocd + 16)? as usize;
    let mut out = Vec::with_capacity(count.min(4096));
    for _ in 0..count {
        if u32_at(bytes, at)? != 0x0201_4b50 {
            return Err("damaged zip archive (bad central directory)".into());
        }
        let flags = u16_at(bytes, at + 8)?;
        let method = u16_at(bytes, at + 10)?;
        let crc = u32_at(bytes, at + 16)?;
        let csize = u32_at(bytes, at + 20)?;
        let size = u32_at(bytes, at + 24)?;
        let name_len = u16_at(bytes, at + 28)? as usize;
        let extra_len = u16_at(bytes, at + 30)? as usize;
        let comment_len = u16_at(bytes, at + 32)? as usize;
        let local = u32_at(bytes, at + 42)? as usize;
        let raw = bytes.get(at + 46..at + 46 + name_len).ok_or("truncated zip archive")?;
        let name = String::from_utf8_lossy(raw).replace('\\', "/");
        at += 46 + name_len + extra_len + comment_len;
        if csize == u32::MAX || size == u32::MAX || local == u32::MAX as usize {
            return Err("zip64 archives are not supported".into());
        }
        if flags & 1 != 0 {
            return Err("encrypted zip archives are not supported".into());
        }
        if u32_at(bytes, local)? != 0x0403_4b50 {
            return Err("damaged zip archive (bad local header)".into());
        }
        let start = local + 30 + u16_at(bytes, local + 26)? as usize + u16_at(bytes, local + 28)? as usize;
        let end = start.checked_add(csize as usize).filter(|&e| e <= bytes.len()).ok_or("truncated zip archive")?;
        out.push(ZipEntry { name, method, crc, size: size as usize, data: start..end });
    }
    Ok(out)
}

/// The contents of `entry` of the archive `bytes`; at most `limit` bytes.
pub fn extract(bytes: &[u8], entry: &ZipEntry, limit: usize) -> Result<Vec<u8>, String> {
    if entry.size > limit {
        return Err("zip entry larger than expected".into());
    }
    let data = bytes.get(entry.data.clone()).ok_or("truncated zip archive")?;
    let out = match entry.method {
        STORED => data.to_vec(),
        DEFLATED => inflate(data, limit)?,
        m => return Err(format!("zip compression method {m} is not supported")),
    };
    if out.len() != entry.size || crc32(&out) != entry.crc {
        return Err("damaged zip archive (checksum mismatch)".into());
    }
    Ok(out)
}

/// The file `name` of the archive (case-insensitive), extracted.
pub fn extract_named(bytes: &[u8], entries: &[ZipEntry], name: &str, limit: usize) -> Option<Result<Vec<u8>, String>> {
    let e = entries.iter().find(|e| e.name.eq_ignore_ascii_case(name))?;
    Some(extract(bytes, e, limit))
}

/// A zip archive being written.
#[derive(Default)]
pub struct ZipWriter {
    out: Vec<u8>,
    central: Vec<u8>,
    count: u16,
}

impl ZipWriter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a file, DEFLATE-compressed when `compress` and it gets smaller.
    pub fn add(&mut self, name: &str, data: &[u8], compress: bool) {
        let packed = if compress { deflate(data) } else { Vec::new() };
        let (method, body) = if compress && packed.len() < data.len() { (DEFLATED, &packed[..]) } else { (STORED, data) };
        let crc = crc32(data);
        let offset = self.out.len() as u32;
        let head = |sig: u32, central: bool| {
            let mut h = sig.to_le_bytes().to_vec();
            if central {
                h.extend_from_slice(&20u16.to_le_bytes());
            }
            h.extend_from_slice(&20u16.to_le_bytes());
            h.extend_from_slice(&0u16.to_le_bytes());
            h.extend_from_slice(&method.to_le_bytes());
            // 1980-01-01 00:00.
            h.extend_from_slice(&0u16.to_le_bytes());
            h.extend_from_slice(&0x21u16.to_le_bytes());
            h.extend_from_slice(&crc.to_le_bytes());
            h.extend_from_slice(&(body.len() as u32).to_le_bytes());
            h.extend_from_slice(&(data.len() as u32).to_le_bytes());
            h.extend_from_slice(&(name.len() as u16).to_le_bytes());
            h.extend_from_slice(&0u16.to_le_bytes());
            h
        };
        self.out.extend_from_slice(&head(0x0403_4b50, false));
        self.out.extend_from_slice(name.as_bytes());
        self.out.extend_from_slice(body);
        let mut c = head(0x0201_4b50, true);
        // Comment length, disk, internal attributes.
        c.extend_from_slice(&[0; 6]);
        c.extend_from_slice(&0u32.to_le_bytes());
        c.extend_from_slice(&offset.to_le_bytes());
        c.extend_from_slice(name.as_bytes());
        self.central.extend_from_slice(&c);
        self.count += 1;
    }

    pub fn finish(mut self) -> Vec<u8> {
        let at = self.out.len() as u32;
        let size = self.central.len() as u32;
        self.out.append(&mut self.central);
        self.out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        self.out.extend_from_slice(&[0; 4]);
        self.out.extend_from_slice(&self.count.to_le_bytes());
        self.out.extend_from_slice(&self.count.to_le_bytes());
        self.out.extend_from_slice(&size.to_le_bytes());
        self.out.extend_from_slice(&at.to_le_bytes());
        self.out.extend_from_slice(&0u16.to_le_bytes());
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archives_round_trip() {
        let xml = "<note><pitch><step>C</step></pitch></note>\n".repeat(200);
        let mut w = ZipWriter::new();
        w.add("mimetype", b"application/vnd.recordare.musicxml", false);
        w.add("META-INF/", b"", false);
        w.add("score.xml", xml.as_bytes(), true);
        let zip = w.finish();
        assert!(is_zip(&zip));
        let entries = read_zip(&zip).unwrap();
        assert_eq!(entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["mimetype", "META-INF/", "score.xml"]);
        assert!(entries[1].is_dir());
        assert_eq!(entries[2].method, DEFLATED);
        assert_eq!(extract(&zip, &entries[2], 1 << 20).unwrap(), xml.as_bytes());
        assert_eq!(extract_named(&zip, &entries, "MIMETYPE", 100).unwrap().unwrap(), b"application/vnd.recordare.musicxml");
        assert!(extract_named(&zip, &entries, "nope", 100).is_none());
        assert!(extract(&zip, &entries[2], 100).is_err(), "limit");
    }

    /// `python3 -c "import zipfile; ...; z.writestr(ZipInfo('a.txt', (1980, 1, 1, 0, 0, 0)), 'hi hi hi hi')"`
    #[test]
    fn reads_a_python_archive() {
        let zip: Vec<u8> = [
            "504b0304140000000800000021003026747a070000000b00000005000000612e747874cbc854c8802200504b",
            "01021403140000000800000021003026747a070000000b000000050000000000000000000000800100000000",
            "612e747874504b05060000000001000100330000002a0000000000",
        ]
        .concat()
        .as_bytes()
        .chunks(2)
        .map(|h| u8::from_str_radix(std::str::from_utf8(h).unwrap(), 16).unwrap())
        .collect();
        let entries = read_zip(&zip).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(extract(&zip, &entries[0], 100).unwrap(), b"hi hi hi hi");
    }

    #[test]
    fn junk_and_damage_are_errors() {
        assert!(read_zip(b"").is_err());
        assert!(read_zip(b"PK\x05\x06").is_err());
        assert!(read_zip(&[0xff; 300]).is_err());
        let mut w = ZipWriter::new();
        w.add("a.xml", "<a/>".repeat(100).as_bytes(), true);
        let zip = w.finish();
        for cut in [10, 40, zip.len() / 2, zip.len() - 1] {
            let _ = read_zip(&zip[..cut]).and_then(|e| e.iter().map(|e| extract(&zip[..cut], e, 1 << 20)).collect::<Result<Vec<_>, _>>());
        }
        let mut bad = zip.clone();
        bad[40] ^= 0x55;
        let entries = read_zip(&bad).unwrap();
        assert!(extract(&bad, &entries[0], 1 << 20).is_err());
    }
}
