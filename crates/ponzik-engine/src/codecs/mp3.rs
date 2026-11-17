//! MPEG-1, MPEG-2 and MPEG-2.5 Layer III ("MP3") decoder, written from
//! ISO/IEC 11172-3 and 13818-3.
//!
//! Frame sync (with ID3v2, ID3v1 and APE tags skipped), CBR, VBR and free
//! format streams, the Xing/Info and VBRI headers and the LAME gapless
//! information (encoder delay and padding are trimmed, the same way FFmpeg
//! does), the bit reservoir, every Huffman table, requantization, mid/side and
//! intensity stereo, reordering, alias reduction, IMDCT (long, short and mixed
//! blocks), overlap-add, frequency inversion and the polyphase synthesis
//! filterbank. A damaged frame decodes to silence instead of failing the file.

use super::Decoded;
use super::mp3_tables::*;
use ponzik_i18n::tr;
use std::f64::consts::PI;
use std::sync::OnceLock;

/// Decoder delay of the hybrid filterbank, added to the LAME encoder delay.
const DECODER_DELAY: usize = 529;
/// The reservoir can point back 511 bytes at most.
const MAX_RESERVOIR: usize = 511;

fn corrupt() -> String {
    tr("Corrupt MP3 file").into()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Version {
    Mpeg1,
    Mpeg2,
    Mpeg25,
}

#[derive(Clone, Copy, Debug)]
struct Header {
    /// The four header bytes.
    raw: [u8; 4],
    version: Version,
    crc: bool,
    /// kbit/s, 0 for free format.
    bitrate: u32,
    sample_rate: u32,
    padding: bool,
    /// 0 stereo, 1 joint stereo, 2 dual channel, 3 mono.
    mode: u8,
    mode_ext: u8,
}

impl Header {
    fn parse(b: &[u8]) -> Option<Header> {
        if b.len() < 4 || b[0] != 0xFF || b[1] & 0xE0 != 0xE0 {
            return None;
        }
        let version = match (b[1] >> 3) & 3 {
            0 => Version::Mpeg25,
            2 => Version::Mpeg2,
            3 => Version::Mpeg1,
            _ => return None,
        };
        // Layer III only.
        if (b[1] >> 1) & 3 != 1 {
            return None;
        }
        let bitrate_index = (b[2] >> 4) as usize;
        let rate_index = ((b[2] >> 2) & 3) as usize;
        if bitrate_index == 15 || rate_index == 3 {
            return None;
        }
        const MPEG1: [u32; 15] = [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320];
        const LSF: [u32; 15] = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];
        let base = [44_100, 48_000, 32_000][rate_index];
        let (bitrate, sample_rate) = match version {
            Version::Mpeg1 => (MPEG1[bitrate_index], base),
            Version::Mpeg2 => (LSF[bitrate_index], base / 2),
            Version::Mpeg25 => (LSF[bitrate_index], base / 4),
        };
        Some(Header {
            raw: [b[0], b[1], b[2], b[3]],
            version,
            crc: b[1] & 1 == 0,
            bitrate,
            sample_rate,
            padding: b[2] & 2 != 0,
            mode: b[3] >> 6,
            mode_ext: (b[3] >> 4) & 3,
        })
    }

    fn mpeg1(&self) -> bool {
        self.version == Version::Mpeg1
    }

    fn channels(&self) -> usize {
        if self.mode == 3 { 1 } else { 2 }
    }

    fn samples(&self) -> usize {
        if self.mpeg1() { 1152 } else { 576 }
    }

    fn granules(&self) -> usize {
        if self.mpeg1() { 2 } else { 1 }
    }

    /// Frame length in bytes; `free` is the slot count of a free-format stream.
    fn frame_bytes(&self, free: usize) -> usize {
        let pad = self.padding as usize;
        if self.bitrate == 0 {
            return free + pad;
        }
        let per = if self.mpeg1() { 144_000 } else { 72_000 };
        (per * self.bitrate / self.sample_rate) as usize + pad
    }

    fn side_info_bytes(&self) -> usize {
        match (self.mpeg1(), self.channels()) {
            (true, 1) => 17,
            (true, _) => 32,
            (false, 1) => 9,
            (false, _) => 17,
        }
    }

    /// Offset of the side information.
    fn side_info_start(&self) -> usize {
        if self.crc { 6 } else { 4 }
    }

    /// Scale factor band tables of the sample rate.
    fn group(&self) -> usize {
        let rate = ((self.raw[2] >> 2) & 3) as usize;
        let k = rate
            + 3 * match self.version {
                Version::Mpeg1 => 2,
                Version::Mpeg2 => 1,
                Version::Mpeg25 => 0,
            };
        k - (k != 0) as usize
    }

    /// Same stream: version, layer, sample rate and free format agree.
    fn compatible(&self, other: &Header) -> bool {
        (self.raw[1] ^ other.raw[1]) & 0xFE == 0 && (self.raw[2] ^ other.raw[2]) & 0x0C == 0 && (self.bitrate == 0) == (other.bitrate == 0)
    }

    fn ms_stereo(&self) -> bool {
        self.mode == 1 && self.mode_ext & 2 != 0
    }

    fn intensity_stereo(&self) -> bool {
        self.mode == 1 && self.mode_ext & 1 != 0
    }
}

/// Big-endian bit reader; reads past the end give zeros.
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Bits { data, pos: 0 }
    }

    /// The next `n` (≤ 32) bits, without consuming them.
    fn peek(&self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        let byte = self.pos / 8;
        let mut word = 0u64;
        for k in 0..8 {
            word = word << 8 | *self.data.get(byte + k).unwrap_or(&0) as u64;
        }
        ((word << (self.pos % 8)) >> (64 - n)) as u32
    }

    fn skip(&mut self, n: u32) {
        self.pos += n as usize;
    }

    fn read(&mut self, n: u32) -> u32 {
        let v = self.peek(n);
        self.skip(n);
        v
    }
}

/// Side information of one granule of one channel.
#[derive(Clone, Copy, Default)]
struct Granule {
    part2_3_length: usize,
    big_values: usize,
    global_gain: u32,
    scalefac_compress: u32,
    block_type: u8,
    mixed: bool,
    table_select: [usize; 3],
    subblock_gain: [u32; 3],
    region0_count: usize,
    region1_count: usize,
    window_switching: bool,
    preflag: bool,
    scalefac_scale: bool,
    count1_table: bool,
    /// MPEG-1 scale factor reuse (one bit per group, group 0 the highest).
    scfsi: u8,
}

struct SideInfo {
    main_data_begin: usize,
    gr: [[Granule; 2]; 2],
}

fn read_side_info(h: &Header, data: &[u8]) -> Option<SideInfo> {
    let mut b = Bits::new(data);
    let nch = h.channels();
    let mut scfsi = [0u8; 2];
    let main_data_begin = if h.mpeg1() {
        let begin = b.read(9) as usize;
        b.skip(if nch == 1 { 5 } else { 3 });
        for s in scfsi.iter_mut().take(nch) {
            *s = b.read(4) as u8;
        }
        begin
    } else {
        let begin = b.read(8) as usize;
        b.skip(nch as u32);
        begin
    };
    let mut gr = [[Granule::default(); 2]; 2];
    for (k, g) in gr.iter_mut().enumerate().take(h.granules()) {
        for (ch, gi) in g.iter_mut().enumerate().take(nch) {
            gi.part2_3_length = b.read(12) as usize;
            gi.big_values = b.read(9) as usize;
            if gi.big_values > 288 {
                return None;
            }
            gi.global_gain = b.read(8);
            gi.scalefac_compress = b.read(if h.mpeg1() { 4 } else { 9 });
            gi.window_switching = b.read(1) == 1;
            if gi.window_switching {
                gi.block_type = b.read(2) as u8;
                if gi.block_type == 0 {
                    return None;
                }
                gi.mixed = b.read(1) == 1;
                for t in 0..2 {
                    gi.table_select[t] = b.read(5) as usize;
                }
                for s in 0..3 {
                    gi.subblock_gain[s] = b.read(3);
                }
            } else {
                for t in 0..3 {
                    gi.table_select[t] = b.read(5) as usize;
                }
                gi.region0_count = b.read(4) as usize;
                gi.region1_count = b.read(3) as usize;
            }
            if h.mpeg1() {
                gi.preflag = b.read(1) == 1;
                // Reuse of the first granule's factors, for long blocks.
                gi.scfsi = if k == 0 || gi.block_type == 2 { 0 } else { scfsi[ch] };
            } else {
                let intensity_right = h.intensity_stereo() && ch == 1;
                gi.preflag = !intensity_right && gi.scalefac_compress >= 500;
            }
            gi.scalefac_scale = b.read(1) == 1;
            gi.count1_table = b.read(1) == 1;
        }
    }
    Some(SideInfo { main_data_begin, gr })
}

/// Scale factor bands of a granule, in bitstream order (short bands appear
/// three times, one per window).
struct Bands {
    widths: Vec<u8>,
    /// Long bands at the start (all for a long block).
    long: usize,
    /// Short bands that follow (multiple of 3).
    short: usize,
}

impl Bands {
    fn of(h: &Header, gi: &Granule) -> Bands {
        let group = h.group();
        if gi.block_type != 2 {
            return Bands { widths: SFB_LONG[group].to_vec(), long: 22, short: 0 };
        }
        let mut widths = Vec::with_capacity(40);
        let first_short = if gi.mixed {
            widths.extend_from_slice(MIXED_LONG[group]);
            if group == 1 { 2 } else { 3 }
        } else {
            0
        };
        let long = widths.len();
        for &w in &SFB_SHORT[group][first_short..] {
            widths.extend_from_slice(&[w, w, w]);
        }
        let short = widths.len() - long;
        Bands { widths, long, short }
    }
}

/// Reads the scale factors of one channel into `scf` (one per band, in
/// bitstream order) and keeps them in `ist_pos` for the next granule's reuse
/// and intensity stereo (255 marks an illegal intensity position).
fn read_scalefactors(h: &Header, gi: &Granule, ch: usize, bands: &Bands, b: &mut Bits, scf: &mut [u8; 40], ist_pos: &mut [u8; 40]) {
    let kind = if bands.short == 0 { 0 } else if bands.long == 0 { 2 } else { 1 };
    let mut partition: &[u8] = &SCF_PARTITIONS[kind];
    let mut size = [0u8; 4];
    let lsf = !h.mpeg1();
    if !lsf {
        let part = SCFC_DECODE[gi.scalefac_compress as usize & 15];
        size = [part >> 2, part >> 2, part & 3, part & 3];
    } else {
        let intensity = h.intensity_stereo() && ch == 1;
        let mut sfc = (gi.scalefac_compress >> intensity as u32) as i32;
        let mut k = if intensity { 12 } else { 0 };
        while sfc >= 0 {
            let mut modprod = 1i32;
            for i in (0..4).rev() {
                let m = LSF_MOD[k + i] as i32;
                size[i] = (sfc / modprod % m) as u8;
                modprod *= m;
            }
            sfc -= modprod;
            k += 4;
        }
        partition = &partition[k..];
    }
    let mut at = 0;
    let mut reuse = gi.scfsi;
    for i in 0..4 {
        let count = partition[i] as usize;
        if count == 0 {
            break;
        }
        let end = (at + count).min(40);
        if !lsf && reuse & 8 != 0 {
            scf[at..end].copy_from_slice(&ist_pos[at..end]);
        } else if size[i] == 0 {
            scf[at..end].fill(0);
            ist_pos[at..end].fill(0);
        } else {
            let max = (1u32 << size[i]) - 1;
            for k in at..end {
                let s = b.read(size[i] as u32);
                ist_pos[k] = if lsf && s == max { 255 } else { s as u8 };
                scf[k] = s as u8;
            }
        }
        reuse <<= 1;
        at = end;
    }
    // The last bands carry no scale factor.
    scf[at.min(40)..(at + 3).min(40)].fill(0);
}

/// `|v|^(4/3)` for the 8 207 possible magnitudes.
fn pow43() -> &'static [f32] {
    static TABLE: OnceLock<Vec<f32>> = OnceLock::new();
    TABLE.get_or_init(|| (0..8207).map(|v| (v as f64).powf(4.0 / 3.0) as f32).collect())
}

fn huffman_pair(b: &mut Bits, table: usize) -> (i32, i32) {
    let base = HUFF_START[table] as i32;
    let mut w = 5;
    let mut leaf = *HUFF.get((base + b.peek(w) as i32) as usize).unwrap_or(&0) as i32;
    while leaf < 0 {
        b.skip(w);
        w = (leaf & 7) as u32;
        let at = base + b.peek(w) as i32 - (leaf >> 3);
        leaf = *HUFF.get(at as usize).unwrap_or(&0) as i32;
    }
    b.skip((leaf >> 8) as u32);
    (leaf & 15, (leaf >> 4) & 15)
}

fn escape(b: &mut Bits, mut v: i32, linbits: u32) -> i32 {
    if v == 15 && linbits > 0 {
        v += b.read(linbits) as i32;
    }
    if v != 0 && b.read(1) == 1 { -v } else { v }
}

/// Huffman-decodes the quantized spectrum of one channel, up to bit `end`.
fn read_spectrum(h: &Header, gi: &Granule, b: &mut Bits, end: usize, out: &mut [i32; 576]) {
    out.fill(0);
    let big = (gi.big_values * 2).min(576);
    let long = &SFB_LONG[h.group()];
    let band_end = |n: usize| long.iter().take(n).map(|&w| w as usize).sum::<usize>();
    let (r0, r1) = if gi.window_switching {
        (if gi.block_type == 2 { 3 * SFB_SHORT[h.group()][..3].iter().map(|&w| w as usize).sum::<usize>() } else { band_end(8) }, 576)
    } else {
        (band_end(gi.region0_count + 1), band_end(gi.region0_count + gi.region1_count + 2))
    };
    let mut i = 0;
    for (region, limit) in [r0.min(big), r1.min(big), big].into_iter().enumerate() {
        let table = gi.table_select[region];
        let linbits = LINBITS[table] as u32;
        while i < limit {
            let (x, y) = huffman_pair(b, table);
            out[i] = escape(b, x, linbits);
            out[i + 1] = escape(b, y, linbits);
            i += 2;
        }
    }
    // Count1 region: quadruples of -1, 0 or 1.
    let table: &[u8] = if gi.count1_table { &COUNT1_B } else { &COUNT1_A };
    while i + 4 <= 576 && b.pos < end {
        let mut leaf = table[b.peek(4) as usize];
        if leaf & 8 == 0 {
            let extra = (leaf & 3) as u32;
            leaf = *table.get((leaf >> 3) as usize + (b.peek(4 + extra) & ((1 << extra) - 1)) as usize).unwrap_or(&0);
        }
        b.skip((leaf & 7) as u32);
        let mut quad = [0i32; 4];
        for (k, q) in quad.iter_mut().enumerate() {
            if leaf & (128 >> k) != 0 {
                *q = if b.read(1) == 1 { -1 } else { 1 };
            }
        }
        // A quadruple that runs past the granule is dropped.
        if b.pos > end {
            break;
        }
        out[i..i + 4].copy_from_slice(&quad);
        i += 4;
    }
}

/// Requantizes the spectrum of one channel: `|is|^(4/3) · 2^(gain/4)`.
fn requantize(gi: &Granule, bands: &Bands, scf: &[u8; 40], is: &[i32; 576], xr: &mut [f32; 576]) {
    let pow = pow43();
    let shift = gi.scalefac_scale as i32 + 1;
    let mut at = 0;
    for (k, &w) in bands.widths.iter().enumerate() {
        let mut s = scf[k] as i32;
        if k < bands.long && gi.preflag && (11..21).contains(&k) && bands.short == 0 {
            s += PRETAB[k - 11] as i32;
        }
        let mut quarters = gi.global_gain as i32 - 210 - (s << shift);
        if k >= bands.long {
            quarters -= 8 * gi.subblock_gain[(k - bands.long) % 3] as i32;
        }
        let gain = (quarters as f64 / 4.0).exp2() as f32;
        let end = (at + w as usize).min(576);
        for j in at..end {
            let v = is[j];
            let m = pow[v.unsigned_abs().min(8206) as usize] * gain;
            xr[j] = if v < 0 { -m } else { m };
        }
        at = end;
    }
}

/// Mid/side and intensity stereo, in place on both channels.
fn stereo(h: &Header, right: &Granule, bands: &Bands, ist_pos: &mut [u8; 40], xr: &mut [[f32; 576]; 2]) {
    let ms = h.ms_stereo();
    let [l, r] = xr;
    let mid_side = |l: &mut [f32], r: &mut [f32]| {
        for (a, b) in l.iter_mut().zip(r.iter_mut()) {
            let (m, s) = (*a, *b);
            *a = (m + s) * std::f32::consts::FRAC_1_SQRT_2;
            *b = (m - s) * std::f32::consts::FRAC_1_SQRT_2;
        }
    };
    if !h.intensity_stereo() {
        if ms {
            mid_side(l, r);
        }
        return;
    }
    // Highest band of each window where the right channel is not zero.
    let n = bands.widths.len();
    let mut top = [-1i32; 3];
    let mut at = 0;
    for (k, &w) in bands.widths.iter().enumerate() {
        let end = (at + w as usize).min(576);
        if r[at..end].iter().any(|&v| v != 0.0) {
            top[k % 3] = k as i32;
        }
        at = end;
    }
    if bands.long > 0 {
        let m = top[0].max(top[1]).max(top[2]);
        top = [m; 3];
    }
    // The last band (no scale factor) follows the one before it.
    let blocks = if bands.short > 0 { 3 } else { 1 };
    let default = if h.mpeg1() { 3 } else { 0 };
    for (i, &t) in top.iter().enumerate().take(blocks) {
        let last = n - blocks + i;
        let prev = last - blocks;
        ist_pos[last] = if t >= prev as i32 { default } else { ist_pos[prev] };
    }
    let max_pos = if h.mpeg1() { 7 } else { 64 };
    let shift = right.scalefac_compress & 1;
    let mut at = 0;
    for (k, &w) in bands.widths.iter().enumerate() {
        let end = (at + w as usize).min(576);
        let pos = ist_pos[k.min(39)] as u32;
        if k as i32 > top[k % 3] && pos < max_pos {
            let (kl, kr) = if h.mpeg1() {
                let ratio = (pos as f64 * PI / 12.0).tan();
                if pos == 6 { (1.0, 0.0) } else { (ratio / (1.0 + ratio), 1.0 / (1.0 + ratio)) }
            } else {
                let k = (-((((pos + 1) >> 1) << shift) as f64) / 4.0).exp2();
                if pos & 1 == 1 { (k, 1.0) } else { (1.0, k) }
            };
            for j in at..end {
                r[j] = l[j] * kr as f32;
                l[j] *= kl as f32;
            }
        } else if ms {
            mid_side(&mut l[at..end], &mut r[at..end]);
        }
        at = end;
    }
}

/// Short bands go from (band, window, line) to (band, line, window) order.
fn reorder(bands: &Bands, xr: &mut [f32; 576]) {
    if bands.short == 0 {
        return;
    }
    let start: usize = bands.widths[..bands.long].iter().map(|&w| w as usize).sum();
    let mut tmp = [0f32; 576];
    let mut src = start;
    let mut dst = 0;
    for triple in bands.widths[bands.long..].chunks_exact(3) {
        let w = triple[0] as usize;
        if src + 3 * w > 576 {
            break;
        }
        for i in 0..w {
            for win in 0..3 {
                tmp[dst] = xr[src + win * w + i];
                dst += 1;
            }
        }
        src += 3 * w;
    }
    xr[start..start + dst].copy_from_slice(&tmp[..dst]);
}

/// Butterflies between the subbands of long blocks.
fn antialias(xr: &mut [f32; 576], boundaries: usize) {
    const C: [f64; 8] = [-0.6, -0.535, -0.33, -0.185, -0.095, -0.041, -0.0142, -0.0037];
    for sb in 1..=boundaries {
        for (i, &c) in C.iter().enumerate() {
            let norm = (1.0 + c * c).sqrt();
            let (cs, ca) = ((1.0 / norm) as f32, (c / norm) as f32);
            let (lo, hi) = (xr[18 * sb - 1 - i], xr[18 * sb + i]);
            xr[18 * sb - 1 - i] = lo * cs - hi * ca;
            xr[18 * sb + i] = hi * cs + lo * ca;
        }
    }
}

/// Cosine tables and windows of the IMDCT and the synthesis filterbank.
struct Transforms {
    /// 36-point IMDCT: `cos(π/72 · (2i + 1 + 18)(2k + 1))`, [i][k].
    long: Vec<[f32; 18]>,
    /// 12-point IMDCT: `cos(π/24 · (2i + 1 + 6)(2k + 1))`, [i][k].
    short: [[f32; 6]; 12],
    /// Windows of block types 0 to 3 (type 2: the 12-point one in 0..12).
    windows: [[f32; 36]; 4],
    /// DCT of the synthesis: `cos(n(2k + 1)π/64)`, [n][k] for n in 0..32.
    dct: Vec<[f32; 32]>,
    window: [f32; 512],
}

fn transforms() -> &'static Transforms {
    static T: OnceLock<Transforms> = OnceLock::new();
    T.get_or_init(|| {
        let long = (0..36)
            .map(|i| std::array::from_fn(|k| (PI / 72.0 * (2 * i + 1 + 18) as f64 * (2 * k + 1) as f64).cos() as f32))
            .collect();
        let short = std::array::from_fn(|i| std::array::from_fn(|k| (PI / 24.0 * (2 * i + 1 + 6) as f64 * (2 * k + 1) as f64).cos() as f32));
        let sin36 = |i: usize| (PI / 36.0 * (i as f64 + 0.5)).sin();
        let sin12 = |i: usize| (PI / 12.0 * (i as f64 + 0.5)).sin();
        let start = |i: usize| match i {
            0..18 => sin36(i),
            18..24 => 1.0,
            24..30 => sin12(i - 18),
            _ => 0.0,
        };
        let stop = |i: usize| match i {
            0..6 => 0.0,
            6..12 => sin12(i - 6),
            12..18 => 1.0,
            _ => sin36(i),
        };
        let short_window = |i: usize| if i < 12 { sin12(i) } else { 0.0 };
        let table = |w: &dyn Fn(usize) -> f64| std::array::from_fn(|i| w(i) as f32);
        let windows = [table(&sin36), table(&start), table(&short_window), table(&stop)];
        let dct = (0..32).map(|n| std::array::from_fn(|k| (n as f64 * (2 * k + 1) as f64 * PI / 64.0).cos() as f32)).collect();
        let window = std::array::from_fn(|i| SYNTH_WINDOW[i] as f32 / 65_536.0);
        Transforms { long, short, windows, dct, window }
    })
}

/// IMDCT, windowing and overlap-add of the 32 subbands of one channel; the
/// result replaces `xr` (subband-major, 18 time samples each).
fn hybrid_synthesis(gi: &Granule, xr: &mut [f32; 576], overlap: &mut [[f32; 18]; 32]) {
    let t = transforms();
    let long_subbands = if gi.block_type != 2 {
        32
    } else if gi.mixed {
        2
    } else {
        0
    };
    for sb in 0..32 {
        let input: [f32; 18] = xr[18 * sb..18 * sb + 18].try_into().expect("18 lines");
        let mut out = [0f32; 36];
        if sb < long_subbands {
            let window = &t.windows[if gi.block_type == 2 { 0 } else { gi.block_type as usize }];
            for (i, o) in out.iter_mut().enumerate() {
                let c = &t.long[i];
                *o = (0..18).map(|k| input[k] * c[k]).sum::<f32>() * window[i];
            }
        } else {
            for w in 0..3 {
                for i in 0..12 {
                    let c = &t.short[i];
                    let y = (0..6).map(|k| input[3 * k + w] * c[k]).sum::<f32>();
                    out[6 + 6 * w + i] += y * t.windows[2][i];
                }
            }
        }
        let prev = &mut overlap[sb];
        for i in 0..18 {
            let mut v = out[i] + prev[i];
            // Frequency inversion of the odd subbands.
            if sb % 2 == 1 && i % 2 == 1 {
                v = -v;
            }
            xr[18 * sb + i] = v;
            prev[i] = out[18 + i];
        }
    }
}

/// Polyphase synthesis filterbank of one channel.
struct Synth {
    /// The last 16 V vectors (64 values each); `cur` is the newest.
    v: [[f32; 64]; 16],
    cur: usize,
}

impl Synth {
    fn new() -> Self {
        Synth { v: [[0.0; 64]; 16], cur: 0 }
    }

    /// 32 subband samples in, 32 PCM samples out.
    fn run(&mut self, s: &[f32; 32], out: &mut [f32]) {
        let t = transforms();
        self.cur = (self.cur + 15) % 16;
        // V[i] = Σ s[k]·cos((16 + i)(2k + 1)π/64), from the 33-point DCT Y(n).
        let mut y = [0f32; 33];
        for (n, row) in t.dct.iter().enumerate() {
            y[n] = row.iter().zip(s).map(|(c, x)| c * x).sum();
        }
        let v = &mut self.v[self.cur];
        for (i, slot) in v.iter_mut().enumerate() {
            *slot = if i <= 16 { y[16 + i] } else { -y[(48i32 - i as i32).unsigned_abs() as usize] };
        }
        for (j, o) in out.iter_mut().enumerate().take(32) {
            let mut sum = 0f32;
            for i in 0..8 {
                sum += self.v[(self.cur + 2 * i) % 16][j] * t.window[64 * i + j];
                sum += self.v[(self.cur + 2 * i + 1) % 16][32 + j] * t.window[64 * i + 32 + j];
            }
            *o = sum;
        }
    }
}

/// Decoder state across frames.
struct Layer3 {
    reservoir: Vec<u8>,
    overlap: [[[f32; 18]; 32]; 2],
    synth: [Synth; 2],
    out: [Vec<f32>; 2],
    stereo_seen: bool,
}

impl Layer3 {
    fn new() -> Self {
        Layer3 { reservoir: Vec::new(), overlap: [[[0.0; 18]; 32]; 2], synth: [Synth::new(), Synth::new()], out: [Vec::new(), Vec::new()], stereo_seen: false }
    }

    fn frame(&mut self, h: &Header, frame: &[u8]) {
        let side_start = h.side_info_start();
        let side_end = side_start + h.side_info_bytes();
        let side = if frame.len() >= side_end { read_side_info(h, &frame[side_start..side_end]) } else { None };
        let main = frame.get(side_end..).unwrap_or(&[]);
        let mut buf = Vec::new();
        let usable = match &side {
            Some(si) if si.main_data_begin <= self.reservoir.len() => {
                buf.extend_from_slice(&self.reservoir[self.reservoir.len() - si.main_data_begin..]);
                buf.extend_from_slice(main);
                true
            }
            _ => false,
        };
        self.reservoir.extend_from_slice(main);
        if self.reservoir.len() > MAX_RESERVOIR {
            self.reservoir.drain(..self.reservoir.len() - MAX_RESERVOIR);
        }
        let nch = h.channels();
        self.stereo_seen |= nch == 2;
        let mut b = Bits::new(&buf);
        let mut ist_pos = [[0u8; 40]; 2];
        for g in 0..h.granules() {
            let mut xr = [[0f32; 576]; 2];
            let mut granules = [Granule::default(); 2];
            if let (Some(si), true) = (&side, usable) {
                granules = si.gr[g];
                let bands = [Bands::of(h, &granules[0]), Bands::of(h, &granules[1])];
                for ch in 0..nch {
                    let gi = &granules[ch];
                    let start = b.pos;
                    let mut scf = [0u8; 40];
                    read_scalefactors(h, gi, ch, &bands[ch], &mut b, &mut scf, &mut ist_pos[ch]);
                    let mut is = [0i32; 576];
                    read_spectrum(h, gi, &mut b, start + gi.part2_3_length, &mut is);
                    requantize(gi, &bands[ch], &scf, &is, &mut xr[ch]);
                    b.pos = start + gi.part2_3_length;
                }
                if nch == 2 {
                    // Both channels share the right channel's band layout for stereo.
                    let right = bands[1].widths.clone();
                    let layout = Bands { widths: right, long: bands[1].long, short: bands[1].short };
                    stereo(h, &granules[1], &layout, &mut ist_pos[1], &mut xr);
                }
                for ch in 0..nch {
                    reorder(&bands[ch], &mut xr[ch]);
                    let gi = &granules[ch];
                    let boundaries = if gi.block_type != 2 {
                        31
                    } else if gi.mixed {
                        1
                    } else {
                        0
                    };
                    antialias(&mut xr[ch], boundaries);
                }
            }
            for ch in 0..nch {
                hybrid_synthesis(&granules[ch], &mut xr[ch], &mut self.overlap[ch]);
                let mut pcm = [0f32; 576];
                for slot in 0..18 {
                    let s: [f32; 32] = std::array::from_fn(|sb| xr[ch][18 * sb + slot]);
                    self.synth[ch].run(&s, &mut pcm[32 * slot..32 * slot + 32]);
                }
                self.out[ch].extend_from_slice(&pcm);
            }
            if nch == 1 {
                let n = self.out[0].len() - self.out[1].len();
                let from = self.out[0].len() - n;
                let (l, r) = self.out.split_at_mut(1);
                r[0].extend_from_slice(&l[0][from..]);
            }
        }
    }
}

/// Size of the ID3v2 tags at the start (several may follow each other).
fn id3v2_len(bytes: &[u8]) -> usize {
    let mut at = 0;
    while bytes.len() >= at + 10 && &bytes[at..at + 3] == b"ID3" {
        let b = &bytes[at..];
        let size = (b[6] as usize & 0x7F) << 21 | (b[7] as usize & 0x7F) << 14 | (b[8] as usize & 0x7F) << 7 | (b[9] as usize & 0x7F);
        let footer = if b[5] & 0x10 != 0 { 10 } else { 0 };
        at += 10 + size + footer;
    }
    at.min(bytes.len())
}

/// End of the audio, before the ID3v1, Lyrics3 and APE tags.
fn audio_end(bytes: &[u8], start: usize) -> usize {
    let mut end = bytes.len();
    loop {
        if end >= start + 128 && &bytes[end - 128..end - 125] == b"TAG" {
            end -= 128;
        } else if end >= start + 32 && &bytes[end - 32..end - 24] == b"APETAGEX" {
            let f = &bytes[end - 32..end];
            let size = u32::from_le_bytes([f[12], f[13], f[14], f[15]]) as usize;
            let header = if f[23] & 0x80 != 0 { 32 } else { 0 };
            let total = size.saturating_add(header);
            if total > end - start {
                return end;
            }
            end -= total;
        } else {
            return end;
        }
    }
}

struct Stream<'a> {
    data: &'a [u8],
    /// Slots of a free-format frame (without padding).
    free: usize,
}

impl Stream<'_> {
    /// Length of a frame of the stream.
    fn frame_len(&self, h: &Header) -> usize {
        h.frame_bytes(self.free)
    }

    /// A frame header at `at` that belongs to the stream of `like` (any
    /// stream if `None`), and whose successor is also one (or the data ends).
    fn frame_at(&mut self, at: usize, like: Option<&Header>) -> Option<Header> {
        let h = Header::parse(self.data.get(at..)?)?;
        if like.is_some_and(|l| !l.compatible(&h)) {
            return None;
        }
        if h.bitrate == 0 && self.free == 0 {
            self.free = self.find_free_size(at, &h)?;
        }
        let len = self.frame_len(&h);
        if len < 4 + h.side_info_bytes() {
            return None;
        }
        let next = at + len;
        if next == self.data.len() {
            return Some(h);
        }
        let n = Header::parse(self.data.get(next..)?)?;
        h.compatible(&n).then_some(h)
    }

    /// Free format: the distance to the next matching header.
    fn find_free_size(&self, at: usize, h: &Header) -> Option<usize> {
        let pad = h.padding as usize;
        let min = 4 + h.side_info_bytes();
        for next in at + min..(at + 2_900).min(self.data.len().saturating_sub(4)) {
            if let Some(n) = Header::parse(&self.data[next..])
                && n.compatible(h)
            {
                return Some(next - at - pad);
            }
        }
        None
    }

    /// The next frame from `at`, resynchronizing past garbage.
    fn sync(&mut self, mut at: usize, like: Option<&Header>) -> Option<(usize, Header)> {
        // Right where the previous frame ended: the header is enough.
        if let (Some(l), Some(h)) = (like, self.data.get(at..).and_then(Header::parse))
            && l.compatible(&h)
            && (h.bitrate != 0 || self.free != 0)
            && at + h.side_info_start() + h.side_info_bytes() <= self.data.len()
        {
            return Some((at, h));
        }
        while at + 4 <= self.data.len() {
            if self.data[at] == 0xFF
                && let Some(h) = self.frame_at(at, like)
            {
                return Some((at, h));
            }
            at += 1;
        }
        None
    }
}

/// Frame count and LAME (encoder delay, padding) of a Xing/Info or VBRI header.
type VbrInfo = (Option<u32>, Option<(usize, usize)>);

/// The Xing/Info or VBRI header of the first frame, if it is one.
fn vbr_header(h: &Header, frame: &[u8]) -> Option<VbrInfo> {
    let be32 = |at: usize| frame.get(at..at + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
    // LAME writes the tag right after the side information even when the
    // header announces a CRC.
    let is_tag = |at: usize| frame.get(at..at + 4).is_some_and(|t| t == b"Xing" || t == b"Info");
    let xing = [4, h.side_info_start()].map(|s| s + h.side_info_bytes()).into_iter().find(|&at| is_tag(at));
    if let Some(xing) = xing {
        let flags = be32(xing + 4)?;
        let mut at = xing + 8;
        let mut frames = None;
        if flags & 1 != 0 {
            frames = be32(at);
            at += 4;
        }
        if flags & 2 != 0 {
            at += 4;
        }
        if flags & 4 != 0 {
            at += 100;
        }
        if flags & 8 != 0 {
            at += 4;
        }
        let gapless = match frame.get(at..at + 24) {
            Some(lame) if [&b"LAME"[..], b"Lavf", b"Lavc"].contains(&&lame[..4]) => {
                let d = &lame[21..24];
                Some(((d[0] as usize) << 4 | (d[1] as usize) >> 4, (d[1] as usize & 15) << 8 | d[2] as usize))
            }
            _ => None,
        };
        return Some((frames, gapless));
    }
    if frame.get(36..40) == Some(b"VBRI") {
        return Some((be32(36 + 14), None));
    }
    None
}

/// True if the bytes look like an MP3 file: an ID3v2 tag, or two consecutive
/// Layer III frames at the start.
pub fn is_mp3(bytes: &[u8]) -> bool {
    if bytes.starts_with(b"ID3") {
        return true;
    }
    let mut s = Stream { data: bytes, free: 0 };
    s.frame_at(0, None).is_some()
}

/// Decodes a whole MP3 file.
pub fn decode(bytes: &[u8]) -> Result<Decoded, String> {
    let start = id3v2_len(bytes);
    let end = audio_end(bytes, start);
    let mut stream = Stream { data: &bytes[start..end], free: 0 };
    let (mut at, first) = stream.sync(0, None).ok_or_else(corrupt)?;
    let mut dec = Layer3::new();
    let mut gapless = None;
    let mut frames_total = None;
    let first_len = stream.frame_len(&first);
    if let Some((frames, lame)) = vbr_header(&first, &stream.data[at..(at + first_len).min(stream.data.len())]) {
        frames_total = frames;
        gapless = lame;
        at += first_len;
    }
    let mut decoded_frames = 0usize;
    while let Some((pos, h)) = stream.sync(at, Some(&first)) {
        let len = stream.frame_len(&h);
        let frame = &stream.data[pos..(pos + len).min(stream.data.len())];
        dec.frame(&h, frame);
        decoded_frames += 1;
        at = pos + len;
    }
    if decoded_frames == 0 {
        return Err(corrupt());
    }
    let [mut left, mut right] = dec.out;
    if let Some((delay, padding)) = gapless {
        let skip = (delay + DECODER_DELAY).min(left.len());
        let mut keep_end = left.len();
        if let Some(frames) = frames_total {
            let total = (frames as usize).saturating_mul(first.samples()) + DECODER_DELAY;
            keep_end = keep_end.min(total.saturating_sub(padding)).max(skip);
        }
        left.truncate(keep_end);
        right.truncate(keep_end);
        left.drain(..skip);
        right.drain(..skip);
    }
    Ok(Decoded { sample_rate: first.sample_rate, channels: if dec.stereo_seen { 2 } else { 1 }, left, right })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codecs::testing::{never_panics, sine_snr};

    /// LAME 64 kbit/s joint stereo, 32 kHz: 16 000 frames of a 440 Hz sine at
    /// 0.5 on the left and a 660 Hz one at 0.3 on the right.
    const LAME: &[u8] = include_bytes!("../../tests/data/lame-sine-32k-stereo.mp3");

    #[test]
    fn lame_file_decodes_to_its_sines_without_the_encoder_delay() {
        assert!(is_mp3(LAME));
        let d = decode(LAME).unwrap();
        assert_eq!((d.sample_rate, d.channels, d.frames()), (32_000, 2, 16_000), "gapless: exactly the encoded length");
        assert!(sine_snr(&d.left, 0.5, 440.0, 32_000.0) > 20.0);
        assert!(sine_snr(&d.right, 0.3, 660.0, 32_000.0) > 20.0);
    }

    #[test]
    fn tags_and_garbage_around_the_frames_are_skipped() {
        let mut file = b"ID3\x04\x00\x00\x00\x00\x00\x05hello".to_vec();
        file.extend_from_slice(&[0xFF, 0xFB, 0x00, 0x13, 0x37]);
        file.extend_from_slice(LAME);
        file.extend_from_slice(b"TAG");
        file.extend_from_slice(&[b'x'; 125]);
        let d = decode(&file).unwrap();
        assert_eq!(d.frames(), 16_000);
        assert!(sine_snr(&d.left, 0.5, 440.0, 32_000.0) > 20.0);
    }

    #[test]
    fn damaged_files_never_panic() {
        never_panics(LAME, |b| {
            let _ = decode(b);
        });
        assert!(decode(b"").is_err());
        assert!(decode(&[0xFF; 4_000]).is_err());
        assert!(!is_mp3(b"RIFF....WAVE"));
    }

    #[test]
    fn headers() {
        // MPEG-1 Layer III, 128 kbit/s, 44.1 kHz, joint stereo.
        let h = Header::parse(&[0xFF, 0xFB, 0x90, 0x64]).unwrap();
        assert_eq!((h.version, h.bitrate, h.sample_rate, h.channels()), (Version::Mpeg1, 128, 44_100, 2));
        assert_eq!(h.frame_bytes(0), 417);
        // MPEG-2.5, 8 kHz.
        let h = Header::parse(&[0xFF, 0xE3, 0x28, 0xC4]).unwrap();
        assert_eq!((h.version, h.sample_rate, h.group(), h.samples()), (Version::Mpeg25, 8_000, 1, 576));
        // Layer II, a bad bitrate, a bad rate.
        assert!(Header::parse(&[0xFF, 0xFD, 0x90, 0x64]).is_none());
        assert!(Header::parse(&[0xFF, 0xFB, 0xF0, 0x64]).is_none());
        assert!(Header::parse(&[0xFF, 0xFB, 0x9C, 0x64]).is_none());
    }
}
