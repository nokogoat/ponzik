//! Encodeur PNG minimal (blocs deflate non compressés), pour les captures d'écran.

fn crc32(data: &[u8], mut crc: u32) -> u32 {
    crc = !crc;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { 0xEDB8_8320 ^ (crc >> 1) } else { crc >> 1 };
        }
    }
    !crc
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&crc32(data, crc32(kind, 0)).to_be_bytes());
}

/// Encode des pixels `0x00RRGGBB` en PNG RGB.
pub fn encode_png(pixels: &[u32], width: usize, height: usize) -> Vec<u8> {
    let mut raw = Vec::with_capacity((width * 3 + 1) * height);
    for row in pixels.chunks_exact(width).take(height) {
        raw.push(0); // filtre « aucun »
        for &p in row {
            raw.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8]);
        }
    }

    let mut z = vec![0x78, 0x01];
    let blocks = raw.chunks(65_535);
    let count = blocks.len();
    for (i, block) in blocks.enumerate() {
        z.push((i + 1 == count) as u8);
        let len = block.len() as u16;
        z.extend_from_slice(&len.to_le_bytes());
        z.extend_from_slice(&(!len).to_le_bytes());
        z.extend_from_slice(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in &raw {
        a = (a + byte as u32) % 65_521;
        b = (b + a) % 65_521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());

    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(width as u32).to_be_bytes());
    ihdr.extend_from_slice(&(height as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_reference() {
        assert_eq!(crc32(b"IEND", 0), 0xAE42_6082);
    }

    #[test]
    fn png_structure() {
        let png = encode_png(&[0xff0000, 0x00ff00, 0x0000ff, 0xffffff], 2, 2);
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&png[png.len() - 12..png.len() - 8], &0u32.to_be_bytes());
    }
}
