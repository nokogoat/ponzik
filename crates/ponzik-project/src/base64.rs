//! Standard base64 (RFC 4648, `+` `/`, `=` padding), for binary blobs in
//! JSON files (plugin states).

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Encodes `data`, padded to a multiple of 4 characters.
pub fn encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn value(c: u8) -> Option<u32> {
    Some(match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => return None,
    } as u32)
}

/// Decodes base64; ASCII whitespace is skipped, padding is optional.
/// `None` on any other character, misplaced padding or a truncated group.
pub fn decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let (mut acc, mut bits, mut pad) = (0u32, 0u32, 0usize);
    for c in text.bytes().filter(|c| !c.is_ascii_whitespace()) {
        if c == b'=' {
            pad += 1;
            continue;
        }
        if pad > 0 {
            return None;
        }
        acc = acc << 6 | value(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    // Leftover bits: 0 (full group), 4 (two characters, `==`), 2 (three
    // characters, `=`); 6 is a lone character. They must be zero, and padding
    // must complete the last group exactly.
    let tail_ok = match bits {
        0 => pad == 0,
        4 => pad == 0 || pad == 2,
        2 => pad == 0 || pad == 1,
        _ => false,
    };
    (tail_ok && acc == 0).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc_4648_vectors() {
        let cases = [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ];
        for (plain, coded) in cases {
            assert_eq!(encode(plain.as_bytes()), coded);
            assert_eq!(decode(coded).as_deref(), Some(plain.as_bytes()), "{coded}");
        }
    }

    #[test]
    fn every_byte_and_length_round_trips() {
        let data: Vec<u8> = (0..=255u8).chain((0..=255u8).rev()).collect();
        for n in 0..data.len() {
            let text = encode(&data[..n]);
            assert_eq!(text.len() % 4, 0);
            assert_eq!(decode(&text).as_deref(), Some(&data[..n]));
        }
        assert!(encode(&[0xfb, 0xff]).contains('+') && encode(&[0xff, 0xff]).contains('/'));
    }

    #[test]
    fn lenient_on_layout_strict_on_content() {
        assert_eq!(decode("Zm9v\nYmE").as_deref(), Some(&b"fooba"[..]), "no padding, line break");
        assert_eq!(decode(" Zg = = ").as_deref(), Some(&b"f"[..]));
        assert_eq!(decode("Zm9v!"), None, "foreign character");
        assert_eq!(decode("Zg==Zg=="), None, "data after padding");
        assert_eq!(decode("Z"), None, "truncated group");
        assert_eq!(decode("Zh=="), None, "non-zero leftover bits");
        assert_eq!(decode("Zg="), None, "half padding");
    }
}
