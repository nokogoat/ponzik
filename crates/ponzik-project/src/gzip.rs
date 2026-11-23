//! DEFLATE (RFC 1951), gzip (RFC 1952) and zlib (RFC 1950), written here:
//! Ableton Live sets (`.als`) are gzipped XML, LMMS projects (`.mmpz`) are
//! zlib streams behind Qt's `qCompress` length, zip archives (`.mxl`) hold
//! DEFLATE entries.
//!
//! The compressor finds repeats with a hash chain over a 32 KiB window and
//! writes them in one fixed-Huffman block: XML shrinks to a fraction of its
//! size, which is all a project file needs. The decompressor reads every block
//! type (stored, fixed and dynamic Huffman), as any gzip tool may write them.

/// CRC-32 (IEEE, reflected, as in gzip and PNG) of `data`.
pub fn crc32(data: &[u8]) -> u32 {
    const TABLE: [u32; 256] = {
        let mut t = [0u32; 256];
        let mut i = 0;
        while i < 256 {
            let mut c = i as u32;
            let mut k = 0;
            while k < 8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
                k += 1;
            }
            t[i] = c;
            i += 1;
        }
        t
    };
    let mut crc = !0u32;
    for &b in data {
        crc = TABLE[((crc ^ b as u32) & 0xff) as usize] ^ (crc >> 8);
    }
    !crc
}

const LEN_BASE: [u16; 29] =
    [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
const LEN_EXTRA: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145,
    8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];

// --- Compression ---

/// Bits written least significant first, as DEFLATE wants them.
struct BitWriter {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl BitWriter {
    fn bits(&mut self, value: u32, count: u32) {
        self.acc |= (value as u64) << self.n;
        self.n += count;
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }

    /// A Huffman code: its bits go most significant first.
    fn code(&mut self, code: u32, len: u32) {
        let rev = code.reverse_bits() >> (32 - len);
        self.bits(rev, len);
    }

    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.out.push(self.acc as u8);
        }
        self.out
    }
}

/// Fixed Huffman code of a literal/length symbol (RFC 1951 §3.2.6).
fn fixed_lit(w: &mut BitWriter, sym: u32) {
    match sym {
        0..=143 => w.code(0x30 + sym, 8),
        144..=255 => w.code(0x190 + sym - 144, 9),
        256..=279 => w.code(sym - 256, 7),
        _ => w.code(0xC0 + sym - 280, 8),
    }
}

fn write_match(w: &mut BitWriter, len: usize, dist: usize) {
    let li = LEN_BASE.iter().rposition(|&b| b as usize <= len).unwrap_or(0);
    fixed_lit(w, 257 + li as u32);
    w.bits((len - LEN_BASE[li] as usize) as u32, LEN_EXTRA[li] as u32);
    let di = DIST_BASE.iter().rposition(|&b| b as usize <= dist).unwrap_or(0);
    w.code(di as u32, 5);
    w.bits((dist - DIST_BASE[di] as usize) as u32, DIST_EXTRA[di] as u32);
}

/// Raw DEFLATE stream of `data`: one fixed-Huffman block with back-references.
pub fn deflate(data: &[u8]) -> Vec<u8> {
    const WINDOW: usize = 32 * 1024;
    const HASH_BITS: u32 = 15;
    const MAX_CHAIN: usize = 48;
    const MAX_LEN: usize = 258;
    let mut w = BitWriter { out: Vec::with_capacity(data.len() / 3 + 16), acc: 0, n: 0 };
    w.bits(1, 1); // last block
    w.bits(1, 2); // fixed Huffman
    let hash = |i: usize| -> usize {
        let v = (data[i] as u32) << 16 | (data[i + 1] as u32) << 8 | data[i + 2] as u32;
        (v.wrapping_mul(0x9E37_79B1) >> (32 - HASH_BITS)) as usize
    };
    let mut head = vec![usize::MAX; 1 << HASH_BITS];
    let mut prev = vec![usize::MAX; WINDOW];
    let insert = |head: &mut [usize], prev: &mut [usize], i: usize| {
        if i + 2 < data.len() {
            let h = hash(i);
            prev[i % WINDOW] = head[h];
            head[h] = i;
        }
    };
    let mut i = 0;
    while i < data.len() {
        let mut best = (0usize, 0usize);
        if i + 2 < data.len() {
            let mut cand = head[hash(i)];
            let mut chain = 0;
            let max = MAX_LEN.min(data.len() - i);
            while cand != usize::MAX && i - cand <= WINDOW && chain < MAX_CHAIN {
                if data[cand + best.0.min(max - 1)] == data[i + best.0.min(max - 1)] {
                    let len = data[cand..].iter().zip(&data[i..i + max]).take_while(|(a, b)| a == b).count();
                    if len > best.0 {
                        best = (len, i - cand);
                        if len == max {
                            break;
                        }
                    }
                }
                let next = prev[cand % WINDOW];
                // The chain only goes back in time; an older slot was overwritten.
                if next == usize::MAX || next >= cand {
                    break;
                }
                cand = next;
                chain += 1;
            }
        }
        if best.0 >= 3 {
            write_match(&mut w, best.0, best.1);
            for k in i..i + best.0 {
                insert(&mut head, &mut prev, k);
            }
            i += best.0;
        } else {
            fixed_lit(&mut w, data[i] as u32);
            insert(&mut head, &mut prev, i);
            i += 1;
        }
    }
    fixed_lit(&mut w, 256);
    w.finish()
}

/// A gzip file of `data` (no name, no time stamp).
pub fn gzip(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 255];
    out.extend_from_slice(&deflate(data));
    out.extend_from_slice(&crc32(data).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out
}

// --- Decompression ---

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    acc: u32,
    n: u32,
}

impl BitReader<'_> {
    fn bits(&mut self, count: u32) -> Result<u32, String> {
        while self.n < count {
            let b = *self.data.get(self.pos).ok_or("truncated DEFLATE data")?;
            self.pos += 1;
            self.acc |= (b as u32) << self.n;
            self.n += 8;
        }
        let v = if count == 0 { 0 } else { self.acc & (u32::MAX >> (32 - count)) };
        self.acc = if count >= 32 { 0 } else { self.acc >> count };
        self.n -= count;
        Ok(v)
    }

    fn align(&mut self) {
        self.acc = 0;
        self.n = 0;
    }
}

/// A canonical Huffman code: how many codes of each length, and the symbols
/// in code order.
struct Huffman {
    count: [u16; 16],
    symbol: Vec<u16>,
}

impl Huffman {
    fn new(lengths: &[u8]) -> Result<Self, String> {
        let mut count = [0u16; 16];
        for &l in lengths {
            count[l as usize] += 1;
        }
        count[0] = 0;
        let mut left = 1i32;
        for &c in &count[1..] {
            left = left * 2 - c as i32;
            if left < 0 {
                return Err("bad Huffman code in DEFLATE data".into());
            }
        }
        let mut offs = [0u16; 16];
        for len in 1..15 {
            offs[len + 1] = offs[len] + count[len];
        }
        let mut symbol = vec![0u16; lengths.len()];
        for (s, &l) in lengths.iter().enumerate() {
            if l != 0 {
                symbol[offs[l as usize] as usize] = s as u16;
                offs[l as usize] += 1;
            }
        }
        Ok(Self { count, symbol })
    }

    fn decode(&self, r: &mut BitReader) -> Result<u16, String> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..16 {
            code |= r.bits(1)? as i32;
            let count = self.count[len] as i32;
            if code - first < count {
                return self.symbol.get((index + code - first) as usize).copied().ok_or_else(|| "bad DEFLATE code".into());
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        Err("bad DEFLATE code".into())
    }
}

/// Decompresses a raw DEFLATE stream; at most `limit` bytes come out.
pub fn inflate(data: &[u8], limit: usize) -> Result<Vec<u8>, String> {
    inflate_at(data, limit).map(|(out, _)| out)
}

/// Like [`inflate`], also returns how many input bytes the stream used.
fn inflate_at(data: &[u8], limit: usize) -> Result<(Vec<u8>, usize), String> {
    let mut r = BitReader { data, pos: 0, acc: 0, n: 0 };
    let mut out: Vec<u8> = Vec::with_capacity((data.len() * 4).min(limit));
    loop {
        let last = r.bits(1)? == 1;
        match r.bits(2)? {
            0 => {
                r.align();
                let head = data.get(r.pos..r.pos + 4).ok_or("truncated DEFLATE data")?;
                let len = u16::from_le_bytes([head[0], head[1]]) as usize;
                if len != !u16::from_le_bytes([head[2], head[3]]) as usize {
                    return Err("bad stored block in DEFLATE data".into());
                }
                r.pos += 4;
                let block = data.get(r.pos..r.pos + len).ok_or("truncated DEFLATE data")?;
                if out.len() + len > limit {
                    return Err("DEFLATE data larger than expected".into());
                }
                out.extend_from_slice(block);
                r.pos += len;
            }
            1 => {
                let mut lengths = [0u8; 288];
                lengths[..144].fill(8);
                lengths[144..256].fill(9);
                lengths[256..280].fill(7);
                lengths[280..].fill(8);
                let lit = Huffman::new(&lengths)?;
                let dist = Huffman::new(&[5u8; 30])?;
                codes(&mut r, &mut out, &lit, &dist, limit)?;
            }
            2 => {
                let nlen = r.bits(5)? as usize + 257;
                let ndist = r.bits(5)? as usize + 1;
                let ncode = r.bits(4)? as usize + 4;
                if nlen > 286 || ndist > 30 {
                    return Err("bad dynamic block in DEFLATE data".into());
                }
                const ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];
                let mut cl = [0u8; 19];
                for &o in &ORDER[..ncode] {
                    cl[o] = r.bits(3)? as u8;
                }
                let clh = Huffman::new(&cl)?;
                let mut lengths = vec![0u8; nlen + ndist];
                let mut i = 0;
                while i < nlen + ndist {
                    let sym = clh.decode(&mut r)?;
                    let (value, repeat) = match sym {
                        0..=15 => (sym as u8, 1),
                        16 => (*lengths.get(i.wrapping_sub(1)).ok_or("bad dynamic block in DEFLATE data")?, 3 + r.bits(2)? as usize),
                        17 => (0, 3 + r.bits(3)? as usize),
                        _ => (0, 11 + r.bits(7)? as usize),
                    };
                    if i + repeat > lengths.len() {
                        return Err("bad dynamic block in DEFLATE data".into());
                    }
                    lengths[i..i + repeat].fill(value);
                    i += repeat;
                }
                if lengths[256] == 0 {
                    return Err("bad dynamic block in DEFLATE data".into());
                }
                let lit = Huffman::new(&lengths[..nlen])?;
                let dist = Huffman::new(&lengths[nlen..])?;
                codes(&mut r, &mut out, &lit, &dist, limit)?;
            }
            _ => return Err("bad block type in DEFLATE data".into()),
        }
        if last {
            return Ok((out, r.pos));
        }
    }
}

fn codes(r: &mut BitReader, out: &mut Vec<u8>, lit: &Huffman, dist: &Huffman, limit: usize) -> Result<(), String> {
    loop {
        let sym = lit.decode(r)? as usize;
        match sym {
            0..=255 => {
                if out.len() >= limit {
                    return Err("DEFLATE data larger than expected".into());
                }
                out.push(sym as u8);
            }
            256 => return Ok(()),
            _ => {
                let li = sym - 257;
                let len = *LEN_BASE.get(li).ok_or("bad length in DEFLATE data")? as usize + r.bits(LEN_EXTRA[li] as u32)? as usize;
                let di = dist.decode(r)? as usize;
                let d = *DIST_BASE.get(di).ok_or("bad distance in DEFLATE data")? as usize + r.bits(DIST_EXTRA[di] as u32)? as usize;
                if d > out.len() {
                    return Err("bad distance in DEFLATE data".into());
                }
                if out.len() + len > limit {
                    return Err("DEFLATE data larger than expected".into());
                }
                let from = out.len() - d;
                for k in 0..len {
                    out.push(out[from + k]);
                }
            }
        }
    }
}

/// Adler-32 (zlib's checksum) of `data`.
pub fn adler32(data: &[u8]) -> u32 {
    const MOD: u32 = 65_521;
    let (mut a, mut b) = (1u32, 0u32);
    // 5552 bytes keep the sums below 2^32 between reductions.
    for chunk in data.chunks(5552) {
        for &x in chunk {
            a += x as u32;
            b += a;
        }
        a %= MOD;
        b %= MOD;
    }
    (b << 16) | a
}

/// True if `bytes` start with a valid zlib header (DEFLATE, no preset dictionary).
pub fn is_zlib(bytes: &[u8]) -> bool {
    match bytes {
        [cmf, flg, ..] => cmf & 0x0f == 8 && cmf >> 4 <= 7 && (*cmf as u16 * 256 + *flg as u16) % 31 == 0 && flg & 0x20 == 0,
        _ => false,
    }
}

/// Decompresses a zlib stream, checking its Adler-32 when it is there.
pub fn zlib_decompress(bytes: &[u8], limit: usize) -> Result<Vec<u8>, String> {
    if !is_zlib(bytes) {
        return Err("not a zlib stream".into());
    }
    let (out, used) = inflate_at(&bytes[2..], limit)?;
    if let Some(tail) = bytes.get(2 + used..2 + used + 4) {
        if u32::from_be_bytes([tail[0], tail[1], tail[2], tail[3]]) != adler32(&out) {
            return Err("damaged zlib stream (checksum mismatch)".into());
        }
    }
    Ok(out)
}

/// A zlib stream of `data`.
pub fn zlib_compress(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x9c];
    out.extend_from_slice(&deflate(data));
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

/// Decompresses Qt's `qCompress` output: the size as 4 big-endian bytes, then
/// a zlib stream (LMMS `.mmpz` projects).
pub fn qt_uncompress(bytes: &[u8], limit: usize) -> Result<Vec<u8>, String> {
    let head = bytes.get(..4).ok_or("truncated compressed data")?;
    let size = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as usize;
    if size > limit {
        return Err("compressed data larger than expected".into());
    }
    let out = zlib_decompress(&bytes[4..], limit)?;
    if out.len() != size {
        return Err("damaged compressed data (size mismatch)".into());
    }
    Ok(out)
}

/// True if `bytes` start like a gzip file.
pub fn is_gzip(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0x1f, 0x8b])
}

/// Decompresses a gzip file (its first member), checking its CRC and size.
pub fn gunzip(bytes: &[u8], limit: usize) -> Result<Vec<u8>, String> {
    if !is_gzip(bytes) || bytes.get(2) != Some(&8) {
        return Err("not a gzip file".into());
    }
    let flags = *bytes.get(3).ok_or("truncated gzip file")?;
    let mut pos = 10;
    if flags & 4 != 0 {
        let x = bytes.get(pos..pos + 2).ok_or("truncated gzip file")?;
        pos += 2 + u16::from_le_bytes([x[0], x[1]]) as usize;
    }
    for flag in [8u8, 16] {
        if flags & flag != 0 {
            let end = bytes.get(pos..).and_then(|b| b.iter().position(|&c| c == 0)).ok_or("truncated gzip file")?;
            pos += end + 1;
        }
    }
    if flags & 2 != 0 {
        pos += 2;
    }
    let body = bytes.get(pos..).ok_or("truncated gzip file")?;
    let (out, used) = inflate_at(body, limit)?;
    let tail = body.get(used..used + 8).ok_or("truncated gzip file")?;
    let crc = u32::from_le_bytes([tail[0], tail[1], tail[2], tail[3]]);
    let size = u32::from_le_bytes([tail[4], tail[5], tail[6], tail[7]]);
    if crc != crc32(&out) || size != out.len() as u32 {
        return Err("damaged gzip file (checksum mismatch)".into());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_of_the_check_string() {
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    fn noise(n: usize, seed: u32) -> Vec<u8> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                (s >> 24) as u8
            })
            .collect()
    }

    #[test]
    fn round_trips() {
        let xml = "<MidiNoteEvent Time=\"0\" Duration=\"0.25\" Velocity=\"100\"/>\n".repeat(2000);
        let cases: Vec<Vec<u8>> = vec![
            Vec::new(),
            b"a".to_vec(),
            b"abcabcabcabcabcabc".to_vec(),
            vec![0; 70_000],
            xml.into_bytes(),
            noise(100_000, 7),
            [noise(40_000, 3), noise(40_000, 3)].concat(),
        ];
        for data in cases {
            let z = deflate(&data);
            assert_eq!(inflate(&z, usize::MAX).unwrap(), data, "{} bytes", data.len());
            let g = gzip(&data);
            assert_eq!(gunzip(&g, usize::MAX).unwrap(), data);
        }
        // Repeats compress.
        let xml = "<KeyTrack Id=\"3\"><Notes/></KeyTrack>".repeat(500);
        assert!(gzip(xml.as_bytes()).len() < xml.len() / 10);
    }

    /// `printf 'hello hello hello hello\n' | gzip -9n`: a fixed block.
    #[test]
    fn reads_gzip_output() {
        let g = [
            0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x03, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0x57, 0xc8, 0x40,
            0x27, 0xb9, 0x00, 0x00, 0x88, 0x59, 0x0b, 0x18, 0x00, 0x00, 0x00,
        ];
        assert_eq!(gunzip(&g, 1 << 20).unwrap(), b"hello hello hello hello\n");
    }

    /// A dynamic Huffman block from zlib (`zlib.compressobj(9, wbits=-15)`
    /// of a text with skewed letters), and a stored block.
    #[test]
    fn reads_dynamic_and_stored_blocks() {
        let text: Vec<u8> = (0..3000u32).map(|i| b"aaaaaaabbbbccd efgh\n"[(i * 7 % 20) as usize]).collect();
        let z = [
            0xed, 0xc8, 0x31, 0x01, 0x00, 0x20, 0x0c, 0xc4, 0xc0, 0x1d, 0x15, 0x58, 0xcb, 0x17, 0x0a, 0xfe, 0x15, 0x60,
            0x83, 0x21, 0x37, 0x1e, 0x99, 0x64, 0x93, 0x26, 0x87, 0xba, 0xd4, 0x60, 0xe1, 0x79, 0x9e, 0xe7, 0x79, 0x9e,
            0xe7, 0x79, 0xde, 0x57, 0xf7, 0x00,
        ];
        assert_eq!(z[0] >> 1 & 3, 2, "a dynamic block");
        assert_eq!(inflate(&z, 1 << 20).unwrap(), text);
        // Stored: header byte, LEN, NLEN, the bytes.
        let mut stored = vec![1u8];
        stored.extend_from_slice(&5u16.to_le_bytes());
        stored.extend_from_slice(&(!5u16).to_le_bytes());
        stored.extend_from_slice(b"hello");
        assert_eq!(inflate(&stored, 100).unwrap(), b"hello");
    }

    #[test]
    fn zlib_and_qt_streams() {
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
        assert_eq!(adler32(b""), 1);
        let big = noise(20_000, 5);
        assert_eq!(adler32(&big), {
            let (mut a, mut b) = (1u64, 0u64);
            for &x in &big {
                a = (a + x as u64) % 65_521;
                b = (b + a) % 65_521;
            }
            (b << 16 | a) as u32
        });
        // `zlib.compress(b"hello hello hello hello\n")`.
        let z = [
            0x78, 0x9c, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0x57, 0xc8, 0x40, 0x27, 0xb9, 0x00, 0x70, 0xbe, 0x08, 0xbb,
        ];
        assert!(is_zlib(&z));
        assert_eq!(zlib_decompress(&z, 1 << 20).unwrap(), b"hello hello hello hello\n");
        let text = "<note pos=\"0\" key=\"57\"/>".repeat(300);
        let mut q = (text.len() as u32).to_be_bytes().to_vec();
        q.extend_from_slice(&zlib_compress(text.as_bytes()));
        assert_eq!(qt_uncompress(&q, 1 << 20).unwrap(), text.as_bytes());
        // Damage: checksum, size, truncation, junk, bombs.
        let mut bad = q.clone();
        let n = bad.len();
        bad[n - 1] ^= 1;
        assert!(qt_uncompress(&bad, 1 << 20).is_err());
        let mut wrong_size = q.clone();
        wrong_size[3] ^= 1;
        assert!(qt_uncompress(&wrong_size, 1 << 20).is_err());
        assert!(qt_uncompress(&q[..q.len() / 2], 1 << 20).is_err());
        assert!(qt_uncompress(&[0, 0], 1 << 20).is_err());
        assert!(zlib_decompress(b"<?xml", 1 << 20).is_err());
        assert!(qt_uncompress(&q, 100).is_err());
    }

    /// PONZIK_GZIP_OUT=<file>: writes a gzipped sample, to check it with `gzip -t`.
    #[test]
    #[ignore]
    fn write_sample_for_gzip_tool() {
        let path = std::env::var("PONZIK_GZIP_OUT").unwrap_or_else(|_| "/tmp/ponzik-sample.gz".into());
        let data: Vec<u8> = [noise(50_000, 9), "<Notes/>".repeat(9000).into_bytes()].concat();
        std::fs::write(path, gzip(&data)).unwrap();
    }

    #[test]
    fn damaged_files_are_errors() {
        let mut g = gzip(b"some text that is long enough to matter");
        assert!(gunzip(&g[..g.len() - 3], 1 << 20).is_err());
        let n = g.len();
        g[n - 6] ^= 1;
        assert!(gunzip(&g, 1 << 20).is_err(), "bad CRC");
        assert!(gunzip(b"not gzip", 1 << 20).is_err());
        assert!(inflate(&[0xff; 40], 1 << 20).is_err());
        // The limit holds against a bomb.
        let big = gzip(&vec![0u8; 1 << 20]);
        assert!(gunzip(&big, 1000).is_err());
    }
}
