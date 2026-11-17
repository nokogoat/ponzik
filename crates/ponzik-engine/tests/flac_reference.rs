//! A file written by the reference encoder (libFLAC 1.4, `flac -8`, 1152-sample
//! blocks): 3 000 stereo 16-bit frames whose raw PCM has a known MD5.

use ponzik_engine::flac::{EncodeOptions, audio_md5, decode, decode_sample, encode};
use ponzik_engine::md5::hex;

const FILE: &[u8] = include_bytes!("data/libflac-16bit-stereo.flac");
/// `md5sum` of the raw little-endian PCM given to `flac`.
const RAW_MD5: &str = "46cd36b0cc62ac4cbd199d97cc30e73e";

#[test]
fn libflac_file_decodes_to_the_known_audio() {
    let s = decode(FILE).expect("decodes");
    assert_eq!((s.sample_rate, s.channels, s.bits, s.frames()), (44_100, 2, 16, 3_000));
    assert_eq!((s.min_block, s.max_block), (1_152, 1_152));
    assert_eq!(hex(&s.md5), RAW_MD5, "STREAMINFO carries the MD5 of the raw audio");
    assert_eq!(hex(&audio_md5(&s.samples, 16)), RAW_MD5, "and the decoded audio matches it");
    assert!(s.md5_ok());
    // The first frame, recomputed: sin(i · 0.031) · 20000 + sin(i · 0.17) · 3000.
    let left = |i: f64| ((i * 0.031).sin() * 20_000.0 + (i * 0.17).sin() * 3_000.0) as i32;
    for i in [0usize, 1, 100, 2_999] {
        assert!((s.samples[2 * i] - left(i as f64)).abs() <= 1, "{i}");
    }
    // Our encoder writes the same audio back, bit for bit.
    let again = decode(&encode(&s.samples, 2, 44_100, 16, EncodeOptions::default())).unwrap();
    assert_eq!(again.samples, s.samples);
    assert_eq!(again.md5, s.md5);
    let sample = decode_sample(FILE, "ref").unwrap();
    assert_eq!(sample.len(), 3_000);
}
