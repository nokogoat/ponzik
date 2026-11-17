//! FLAC, without dependencies.
//!
//! Encoder: fixed blocks; each channel tries a constant, verbatim, fixed
//! (orders 0 to 4) and LPC subframe (order picked from the Levinson-Durbin
//! error, quantized coefficients); partitioned Rice coding of the residual;
//! stereo decorrelation (left/side, right/side or mid/side, whichever is
//! smallest); STREAMINFO with the MD5 of the audio.
//!
//! Decoder: every stream the format allows from 4 to 32 bits (fixed or
//! variable block sizes, wasted bits, escaped partitions, metadata blocks and
//! a leading ID3v2 tag skipped), CRC-8 and CRC-16 checked.

use ponzik_i18n::tr;

use crate::md5::Md5;
use crate::sample::Sample;

/// Encoder settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EncodeOptions {
    /// Samples per channel in each frame (16 to 65 535).
    pub block_size: usize,
    /// Highest LPC order tried (0: fixed predictors only, at most 32).
    pub max_lpc_order: usize,
    /// Highest Rice partition order (at most 15).
    pub max_partition_order: u32,
    /// Try the stereo decorrelations.
    pub stereo: bool,
}

impl Default for EncodeOptions {
    fn default() -> Self {
        Self { block_size: 4_096, max_lpc_order: 12, max_partition_order: 8, stereo: true }
    }
}

/// A decoded stream: interleaved samples, as integers of `bits` bits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stream {
    pub sample_rate: u32,
    pub channels: usize,
    pub bits: u32,
    pub samples: Vec<i32>,
    /// MD5 from STREAMINFO (zeros: not given).
    pub md5: [u8; 16],
    pub min_block: usize,
    pub max_block: usize,
}

impl Stream {
    /// Samples per channel.
    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels.max(1)
    }

    /// True when STREAMINFO has no signature or it matches the audio.
    pub fn md5_ok(&self) -> bool {
        self.md5 == [0; 16] || self.md5 == audio_md5(&self.samples, self.bits)
    }
}

/// MD5 of interleaved samples as FLAC defines it: little-endian, signed, on
/// as many bytes as `bits` needs.
pub fn audio_md5(samples: &[i32], bits: u32) -> [u8; 16] {
    let width = bits.div_ceil(8).clamp(1, 4) as usize;
    let mut h = Md5::new();
    let mut buf = Vec::with_capacity(4 * 1_024);
    for chunk in samples.chunks(1_024) {
        buf.clear();
        for &s in chunk {
            buf.extend_from_slice(&s.to_le_bytes()[..width]);
        }
        h.update(&buf);
    }
    h.finish()
}

/// True if `bytes` looks like a FLAC file (an ID3v2 tag may come first).
pub fn is_flac(bytes: &[u8]) -> bool {
    bytes.get(skip_id3(bytes)..).is_some_and(|b| b.starts_with(b"fLaC"))
}

fn skip_id3(bytes: &[u8]) -> usize {
    if bytes.len() >= 10 && &bytes[..3] == b"ID3" {
        let size = bytes[6..10].iter().fold(0usize, |s, &b| (s << 7) | (b & 0x7f) as usize);
        let footer = if bytes[5] & 0x10 != 0 { 10 } else { 0 };
        10 + size + footer
    } else {
        0
    }
}

// --- CRC ---

const fn crc8_table() -> [u8; 256] {
    let mut t = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u8;
        let mut k = 0;
        while k < 8 {
            c = if c & 0x80 != 0 { (c << 1) ^ 0x07 } else { c << 1 };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
}

const fn crc16_table() -> [u16; 256] {
    let mut t = [0u16; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = (i as u16) << 8;
        let mut k = 0;
        while k < 8 {
            c = if c & 0x8000 != 0 { (c << 1) ^ 0x8005 } else { c << 1 };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
}

const CRC8: [u8; 256] = crc8_table();
const CRC16: [u16; 256] = crc16_table();

fn crc8(data: &[u8]) -> u8 {
    data.iter().fold(0u8, |c, &b| CRC8[(c ^ b) as usize])
}

fn crc16(data: &[u8]) -> u16 {
    data.iter().fold(0u16, |c, &b| (c << 8) ^ CRC16[((c >> 8) as u8 ^ b) as usize])
}

// --- Bits ---

/// MSB-first bit writer.
#[derive(Default)]
struct BitWriter {
    bytes: Vec<u8>,
    acc: u64,
    /// Bits waiting in `acc` (< 8 between calls).
    n: u32,
}

impl BitWriter {
    /// Writes the low `bits` bits of `v` (`bits` ≤ 32).
    fn put(&mut self, v: u64, bits: u32) {
        debug_assert!(bits <= 32);
        if bits == 0 {
            return;
        }
        self.acc = (self.acc << bits) | (v & ((1u64 << bits) - 1));
        self.n += bits;
        while self.n >= 8 {
            self.n -= 8;
            self.bytes.push((self.acc >> self.n) as u8);
        }
        self.acc &= (1u64 << self.n) - 1;
    }

    fn signed(&mut self, v: i64, bits: u32) {
        self.put(v as u64, bits);
    }

    /// `q` zeros then a one.
    fn unary(&mut self, mut q: u64) {
        while q >= 32 {
            self.put(0, 32);
            q -= 32;
        }
        self.put(1, q as u32 + 1);
    }

    fn align(&mut self) {
        if self.n > 0 {
            self.put(0, 8 - self.n);
        }
    }
}

/// MSB-first bit reader.
struct BitReader<'a> {
    data: &'a [u8],
    /// Position in bits.
    pos: usize,
}

type R<T> = Result<T, String>;

fn corrupt() -> String {
    tr("Corrupt FLAC file").into()
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8], byte: usize) -> Self {
        Self { data, pos: byte * 8 }
    }

    fn byte_pos(&self) -> usize {
        self.pos / 8
    }

    /// Reads `bits` (≤ 64) bits.
    fn get(&mut self, bits: u32) -> R<u64> {
        if bits == 0 {
            return Ok(0);
        }
        if self.pos + bits as usize > self.data.len() * 8 {
            return Err(corrupt());
        }
        let mut v = 0u64;
        let mut left = bits;
        while left > 0 {
            let byte = self.data[self.pos / 8];
            let used = (self.pos % 8) as u32;
            let take = (8 - used).min(left);
            let chunk = (byte >> (8 - used - take)) & ((1u16 << take) - 1) as u8;
            v = (v << take) | chunk as u64;
            self.pos += take as usize;
            left -= take;
        }
        Ok(v)
    }

    fn signed(&mut self, bits: u32) -> R<i64> {
        if bits == 0 {
            return Ok(0);
        }
        let v = self.get(bits)?;
        Ok(((v << (64 - bits)) as i64) >> (64 - bits))
    }

    fn unary(&mut self) -> R<u64> {
        let mut q = 0u64;
        loop {
            let Some(&byte) = self.data.get(self.pos / 8) else { return Err(corrupt()) };
            let used = self.pos % 8;
            let rest = byte << used;
            if rest == 0 {
                q += (8 - used) as u64;
                self.pos += 8 - used;
                continue;
            }
            let z = rest.leading_zeros() as usize;
            q += z as u64;
            self.pos += z + 1;
            return Ok(q);
        }
    }

    fn align(&mut self) {
        self.pos = self.pos.div_ceil(8) * 8;
    }
}

// --- Encoder ---

/// Residual coding: partition order, Rice parameter of each partition, and
/// the estimated cost in bits that chose them.
struct Rice {
    order: u32,
    params: Vec<u32>,
    bits: u64,
}

fn fold(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

/// Best partitioned Rice coding of `res` (the residual of a block of `n`
/// samples whose first `pred` samples are warm-up).
fn plan_rice(res: &[i64], n: usize, pred: usize, max_order: u32) -> Rice {
    // Deepest order: partitions that divide the block and outlast the warm-up.
    let mut top = 0;
    while top < max_order.min(15) && n % (1 << (top + 1)) == 0 && (n >> (top + 1)) > pred {
        top += 1;
    }
    let parts = 1usize << top;
    let size = n >> top;
    // Sum of the folded residual of each finest partition.
    let mut sums = vec![0u64; parts];
    let mut counts = vec![0usize; parts];
    for (i, &r) in res.iter().enumerate() {
        let p = (i + pred) / size;
        sums[p] += fold(r);
        counts[p] += 1;
    }
    let mut best: Option<Rice> = None;
    let mut order = top;
    loop {
        let mut params = Vec::with_capacity(sums.len());
        let mut bits = 0u64;
        for (&sum, &count) in sums.iter().zip(&counts) {
            let (k, cost) = best_param(sum, count);
            params.push(k);
            bits += cost;
        }
        let wide = params.iter().any(|&k| k > 14);
        bits += 6 + params.len() as u64 * if wide { 5 } else { 4 };
        if best.as_ref().is_none_or(|b| bits < b.bits) {
            best = Some(Rice { order, params, bits });
        }
        if order == 0 {
            break;
        }
        order -= 1;
        sums = sums.chunks(2).map(|c| c[0] + c[1]).collect();
        counts = counts.chunks(2).map(|c| c[0] + c[1]).collect();
    }
    best.expect("at least order 0")
}

/// Rice parameter for `count` values whose folded sum is `sum`, and its
/// estimated cost in bits.
fn best_param(sum: u64, count: usize) -> (u32, u64) {
    if count == 0 {
        return (0, 0);
    }
    let n = count as u64;
    let cost = |k: u32| n * (k as u64 + 1) + (sum >> k);
    // Near log2 of the mean, the cost is at its lowest.
    let mean = sum / n;
    let guess = if mean == 0 { 0 } else { (63 - mean.leading_zeros()).min(30) };
    let mut best = (0, u64::MAX);
    for k in guess.saturating_sub(1)..=(guess + 1).min(30) {
        let c = cost(k);
        if c < best.1 {
            best = (k, c);
        }
    }
    best
}

fn write_rice(w: &mut BitWriter, res: &[i64], n: usize, pred: usize, rice: &Rice) {
    let wide = rice.params.iter().any(|&k| k > 14);
    w.put(wide as u64, 2);
    w.put(rice.order as u64, 4);
    let size = n >> rice.order;
    let mut at = 0;
    for (p, &k) in rice.params.iter().enumerate() {
        w.put(k as u64, if wide { 5 } else { 4 });
        let count = if p == 0 { size - pred } else { size };
        for &r in &res[at..at + count] {
            let u = fold(r);
            w.unary(u >> k);
            if k > 0 {
                w.put(u & ((1u64 << k) - 1), k);
            }
        }
        at += count;
    }
}

enum Kind {
    Constant,
    Verbatim,
    Fixed(usize),
    Lpc { coefs: Vec<i32>, precision: u32, shift: u32 },
}

/// A subframe ready to write, and its size in bits.
struct Subframe {
    kind: Kind,
    residual: Vec<i64>,
    rice: Option<Rice>,
    bits: u64,
}

fn fixed_residual(x: &[i64], order: usize) -> Vec<i64> {
    (order..x.len())
        .map(|i| match order {
            0 => x[i],
            1 => x[i] - x[i - 1],
            2 => x[i] - 2 * x[i - 1] + x[i - 2],
            3 => x[i] - 3 * x[i - 1] + 3 * x[i - 2] - x[i - 3],
            _ => x[i] - 4 * x[i - 1] + 6 * x[i - 2] - 4 * x[i - 3] + x[i - 4],
        })
        .collect()
}

fn lpc_residual(x: &[i64], coefs: &[i32], shift: u32) -> Vec<i64> {
    let order = coefs.len();
    (order..x.len())
        .map(|i| {
            let sum: i64 = coefs.iter().enumerate().map(|(j, &c)| c as i64 * x[i - 1 - j]).sum();
            x[i] - (sum >> shift)
        })
        .collect()
}

/// Residuals must fit the 32-bit integers decoders use.
fn fits(res: &[i64]) -> bool {
    res.iter().all(|&r| r > i32::MIN as i64 && r <= i32::MAX as i64)
}

/// Prediction coefficients of every order up to `max` (Levinson-Durbin on
/// the windowed autocorrelation), with the prediction error of each.
fn levinson(x: &[i64], max: usize) -> Vec<(Vec<f64>, f64)> {
    let n = x.len();
    // Tukey window (half of the block tapered).
    let taper = (n / 4).max(1);
    let w = |i: usize| {
        let d = i.min(n - 1 - i);
        if d >= taper { 1.0 } else { 0.5 - 0.5 * (std::f64::consts::PI * d as f64 / taper as f64).cos() }
    };
    let data: Vec<f64> = x.iter().enumerate().map(|(i, &v)| v as f64 * w(i)).collect();
    let r: Vec<f64> = (0..=max).map(|lag| (lag..n).map(|i| data[i] * data[i - lag]).sum()).collect();
    let mut out = Vec::with_capacity(max);
    if r[0] <= 0.0 {
        return out;
    }
    let mut a: Vec<f64> = Vec::with_capacity(max);
    let mut err = r[0];
    for m in 1..=max {
        let acc = r[m] - (1..m).map(|k| a[k - 1] * r[m - k]).sum::<f64>();
        let k = acc / err;
        let prev = a.clone();
        a.push(k);
        for j in 1..m {
            a[j - 1] = prev[j - 1] - k * prev[m - j - 1];
        }
        err *= 1.0 - k * k;
        if !err.is_finite() || err <= 0.0 {
            out.push((a.clone(), 0.0));
            break;
        }
        out.push((a.clone(), err));
    }
    out
}

/// Coefficients quantized on `precision` bits (sign included) with the
/// rounding error carried along; `None` if they need a negative shift.
fn quantize(a: &[f64], precision: u32) -> Option<(Vec<i32>, u32)> {
    let cmax = a.iter().fold(0.0f64, |m, c| m.max(c.abs()));
    if cmax <= 0.0 || !cmax.is_finite() {
        return None;
    }
    let p = precision - 1;
    // cmax = m × 2^e with 0.5 ≤ m < 1.
    let e = cmax.log2().floor() as i32 + 1;
    let shift = (p as i32 - e).min(15);
    if shift < 0 {
        return None;
    }
    let (lo, hi) = (-(1i64 << p), (1i64 << p) - 1);
    let mut err = 0.0;
    let coefs = a
        .iter()
        .map(|&c| {
            err += c * (1i64 << shift) as f64;
            let q = (err.round() as i64).clamp(lo, hi);
            err -= q as f64;
            q as i32
        })
        .collect();
    Some((coefs, shift as u32))
}

/// Coefficient precision for a block size (as libFLAC picks it).
fn coef_precision(n: usize, bits: u32) -> u32 {
    let p = match n {
        0..=192 => 7,
        193..=384 => 8,
        385..=576 => 9,
        577..=1_152 => 10,
        1_153..=2_304 => 11,
        2_305..=4_608 => 12,
        _ => 13,
    };
    if bits <= 16 { (p + 2).min(15) } else { p }
}

fn plan_subframe(x: &[i64], bits: u32, opts: &EncodeOptions) -> Subframe {
    let n = x.len();
    let header = 8u64;
    if x.iter().all(|&v| v == x[0]) {
        return Subframe { kind: Kind::Constant, residual: Vec::new(), rice: None, bits: header + bits as u64 };
    }
    let mut best = Subframe { kind: Kind::Verbatim, residual: Vec::new(), rice: None, bits: header + bits as u64 * n as u64 };
    let mut consider = |kind: Kind, pred: usize, extra: u64, residual: Vec<i64>| {
        if !fits(&residual) {
            return;
        }
        let rice = plan_rice(&residual, n, pred, opts.max_partition_order);
        let total = header + pred as u64 * bits as u64 + extra + rice_size(&residual, n, pred, &rice);
        if total < best.bits {
            best = Subframe { kind, residual, rice: Some(rice), bits: total };
        }
    };
    for order in 0..=4.min(n - 1) {
        consider(Kind::Fixed(order), order, 0, fixed_residual(x, order));
    }
    let max = opts.max_lpc_order.min(32).min(n.saturating_sub(1));
    if max > 0 {
        let sets = levinson(x, max);
        // Order with the smallest estimated size (bits per residual sample
        // from the prediction error), then its neighbors.
        let scale = 0.5 / n as f64;
        let precision = coef_precision(n, bits);
        let estimate = |order: usize, err: f64| {
            let per = if err > 0.0 { (0.5 * (scale * err).log2()).max(0.0) } else { 0.0 };
            (bits as f64 + precision as f64) * order as f64 + per * (n - order) as f64
        };
        if let Some(pick) = (0..sets.len()).min_by(|&a, &b| estimate(a + 1, sets[a].1).total_cmp(&estimate(b + 1, sets[b].1))) {
            for idx in [pick, sets.len() - 1] {
                let order = idx + 1;
                if let Some((coefs, shift)) = quantize(&sets[idx].0, precision) {
                    let residual = lpc_residual(x, &coefs, shift);
                    let extra = 4 + 5 + precision as u64 * order as u64;
                    consider(Kind::Lpc { coefs, precision, shift }, order, extra, residual);
                }
                if pick == sets.len() - 1 {
                    break;
                }
            }
        }
    }
    best
}

/// Exact size of the residual coding, quotients included.
fn rice_size(res: &[i64], n: usize, pred: usize, rice: &Rice) -> u64 {
    let wide = rice.params.iter().any(|&k| k > 14);
    let mut bits = 6 + rice.params.len() as u64 * if wide { 5 } else { 4 };
    let size = n >> rice.order;
    let mut at = 0;
    for (p, &k) in rice.params.iter().enumerate() {
        let count = if p == 0 { size - pred } else { size };
        bits += res[at..at + count].iter().map(|&r| (fold(r) >> k) + 1 + k as u64).sum::<u64>();
        at += count;
    }
    bits
}

fn write_subframe(w: &mut BitWriter, x: &[i64], bits: u32, s: &Subframe) {
    let n = x.len();
    w.put(0, 1);
    match &s.kind {
        Kind::Constant => {
            w.put(0, 6);
            w.put(0, 1);
            w.signed(x[0], bits);
        }
        Kind::Verbatim => {
            w.put(1, 6);
            w.put(0, 1);
            for &v in x {
                w.signed(v, bits);
            }
        }
        Kind::Fixed(order) => {
            w.put(8 | *order as u64, 6);
            w.put(0, 1);
            for &v in &x[..*order] {
                w.signed(v, bits);
            }
            write_rice(w, &s.residual, n, *order, s.rice.as_ref().expect("rice"));
        }
        Kind::Lpc { coefs, precision, shift } => {
            let order = coefs.len();
            w.put(32 | (order as u64 - 1), 6);
            w.put(0, 1);
            for &v in &x[..order] {
                w.signed(v, bits);
            }
            w.put(*precision as u64 - 1, 4);
            w.put(*shift as u64, 5);
            for &c in coefs {
                w.signed(c as i64, *precision);
            }
            write_rice(w, &s.residual, n, order, s.rice.as_ref().expect("rice"));
        }
    }
}

/// Frame number, coded like UTF-8 (up to 36 bits).
fn put_utf8(w: &mut BitWriter, v: u64) {
    if v < 0x80 {
        w.put(v, 8);
        return;
    }
    let mut extra = 1;
    while extra < 6 && v >= 1u64 << (6 + 5 * extra) {
        extra += 1;
    }
    let lead_bits = 6 - extra as u32;
    let lead = (0xFFu64 << (7 - extra)) & 0xFF;
    w.put(lead | (v >> (6 * extra)) & ((1u64 << lead_bits) - 1), 8);
    for k in (0..extra).rev() {
        w.put(0x80 | ((v >> (6 * k)) & 0x3F), 8);
    }
}

fn block_size_code(n: usize) -> (u64, Option<(u64, u32)>) {
    match n {
        192 => (1, None),
        576 | 1_152 | 2_304 | 4_608 => (2 + (n / 576).trailing_zeros() as u64, None),
        256 | 512 | 1_024 | 2_048 | 4_096 | 8_192 | 16_384 | 32_768 => (8 + (n / 256).trailing_zeros() as u64, None),
        _ if n <= 256 => (6, Some((n as u64 - 1, 8))),
        _ => (7, Some((n as u64 - 1, 16))),
    }
}

fn sample_rate_code(sr: u32) -> u64 {
    match sr {
        88_200 => 1,
        176_400 => 2,
        192_000 => 3,
        8_000 => 4,
        16_000 => 5,
        22_050 => 6,
        24_000 => 7,
        32_000 => 8,
        44_100 => 9,
        48_000 => 10,
        96_000 => 11,
        // From STREAMINFO.
        _ => 0,
    }
}

fn sample_size_code(bits: u32) -> u64 {
    match bits {
        8 => 1,
        12 => 2,
        16 => 4,
        20 => 5,
        24 => 6,
        32 => 7,
        _ => 0,
    }
}

/// Encodes interleaved integer samples of `bits` bits (4 to 24) as a FLAC
/// file. Samples outside the range of `bits` are clamped.
pub fn encode(samples: &[i32], channels: usize, sample_rate: u32, bits: u32, opts: EncodeOptions) -> Vec<u8> {
    let channels = channels.clamp(1, 8);
    let bits = bits.clamp(4, 24);
    let sample_rate = sample_rate.clamp(1, (1 << 20) - 1);
    let block = opts.block_size.clamp(16, 65_535);
    let (lo, hi) = (-(1i32 << (bits - 1)), (1i32 << (bits - 1)) - 1);
    let samples: Vec<i32> = samples.iter().map(|&s| s.clamp(lo, hi)).collect();
    let frames = samples.len() / channels;
    let samples = &samples[..frames * channels];

    let mut out = Vec::with_capacity(samples.len() * bits as usize / 16 + 128);
    out.extend_from_slice(b"fLaC");
    // STREAMINFO, the last metadata block; frame sizes filled in at the end.
    out.push(0x80);
    out.extend_from_slice(&[0, 0, 34]);
    let info = out.len();
    out.extend_from_slice(&[0; 34]);

    let (mut min_frame, mut max_frame) = (usize::MAX, 0usize);
    let mut chans: Vec<Vec<i64>> = vec![Vec::with_capacity(block); channels];
    for (number, start) in (0..frames).step_by(block).enumerate() {
        let n = block.min(frames - start);
        for (c, ch) in chans.iter_mut().enumerate() {
            ch.clear();
            ch.extend((start..start + n).map(|i| samples[i * channels + c] as i64));
        }
        let frame = encode_frame(&chans, number as u64, n, sample_rate, bits, &opts);
        min_frame = min_frame.min(frame.len());
        max_frame = max_frame.max(frame.len());
        out.extend_from_slice(&frame);
    }

    let mut w = BitWriter::default();
    w.put(block as u64, 16);
    w.put(block as u64, 16);
    w.put(if max_frame == 0 { 0 } else { min_frame as u64 }, 24);
    w.put(max_frame as u64, 24);
    w.put(sample_rate as u64, 20);
    w.put(channels as u64 - 1, 3);
    w.put(bits as u64 - 1, 5);
    w.put((frames as u64) >> 32, 4);
    w.put(frames as u64 & 0xFFFF_FFFF, 32);
    w.bytes.extend_from_slice(&audio_md5(samples, bits));
    out[info..info + 34].copy_from_slice(&w.bytes);
    out
}

fn encode_frame(chans: &[Vec<i64>], number: u64, n: usize, sample_rate: u32, bits: u32, opts: &EncodeOptions) -> Vec<u8> {
    let mut w = BitWriter::default();
    w.put(0xFFF8, 16);
    let (bs_code, bs_extra) = block_size_code(n);
    let sr_code = sample_rate_code(sample_rate);

    // Channel assignment: independent, or the smallest stereo decorrelation.
    let mut assignment = chans.len() as u64 - 1;
    let mut planned: Vec<(Vec<i64>, u32, Subframe)> = Vec::new();
    if chans.len() == 2 && opts.stereo {
        let (l, r) = (&chans[0], &chans[1]);
        let side: Vec<i64> = l.iter().zip(r).map(|(a, b)| a - b).collect();
        let mid: Vec<i64> = l.iter().zip(r).map(|(a, b)| (a + b) >> 1).collect();
        let pl = plan_subframe(l, bits, opts);
        let pr = plan_subframe(r, bits, opts);
        let ps = plan_subframe(&side, bits + 1, opts);
        let pm = plan_subframe(&mid, bits, opts);
        let costs = [pl.bits + pr.bits, pl.bits + ps.bits, ps.bits + pr.bits, pm.bits + ps.bits];
        let pick = (0..4).min_by_key(|&k| costs[k]).unwrap_or(0);
        let (a, b) = match pick {
            0 => ((l.clone(), bits, pl), (r.clone(), bits, pr)),
            1 => ((l.clone(), bits, pl), (side, bits + 1, ps)),
            2 => ((side, bits + 1, ps), (r.clone(), bits, pr)),
            _ => ((mid, bits, pm), (side, bits + 1, ps)),
        };
        assignment = if pick == 0 { 1 } else { 7 + pick as u64 };
        planned.push(a);
        planned.push(b);
    } else {
        for c in chans {
            planned.push((c.clone(), bits, plan_subframe(c, bits, opts)));
        }
    }

    w.put(bs_code, 4);
    w.put(sr_code, 4);
    w.put(assignment, 4);
    w.put(sample_size_code(bits), 3);
    w.put(0, 1);
    put_utf8(&mut w, number);
    if let Some((v, b)) = bs_extra {
        w.put(v, b);
    }
    let crc = crc8(&w.bytes);
    w.put(crc as u64, 8);

    for (x, b, s) in &planned {
        write_subframe(&mut w, x, *b, s);
    }
    w.align();
    let crc = crc16(&w.bytes);
    w.put(crc as u64, 16);
    w.bytes
}

// --- Decoder ---

fn unsupported() -> String {
    tr("Unsupported FLAC file").into()
}

/// Decodes a FLAC file (checksums verified, MD5 left to [`Stream::md5_ok`]).
pub fn decode(bytes: &[u8]) -> R<Stream> {
    let mut pos = skip_id3(bytes);
    if bytes.get(pos..pos + 4) != Some(b"fLaC") {
        return Err(tr("Not a FLAC file").into());
    }
    pos += 4;
    let mut info = None;
    loop {
        let head = bytes.get(pos..pos + 4).ok_or_else(corrupt)?;
        let last = head[0] & 0x80 != 0;
        let kind = head[0] & 0x7f;
        let len = u32::from_be_bytes([0, head[1], head[2], head[3]]) as usize;
        let body = bytes.get(pos + 4..pos + 4 + len).ok_or_else(corrupt)?;
        if kind == 0 {
            if len < 34 {
                return Err(corrupt());
            }
            let mut r = BitReader::new(body, 0);
            let min_block = r.get(16)? as usize;
            let max_block = r.get(16)? as usize;
            r.get(48)?;
            let sample_rate = r.get(20)? as u32;
            let channels = r.get(3)? as usize + 1;
            let bits = r.get(5)? as u32 + 1;
            let total = r.get(36)?;
            let md5: [u8; 16] = body[18..34].try_into().map_err(|_| corrupt())?;
            info = Some((min_block, max_block, sample_rate, channels, bits, total, md5));
        }
        pos += 4 + len;
        if last {
            break;
        }
    }
    let (min_block, max_block, sample_rate, channels, bits, total, md5) = info.ok_or_else(corrupt)?;
    if bits < 4 {
        return Err(unsupported());
    }
    let mut samples: Vec<i32> = Vec::with_capacity((total as usize).min(1 << 28) * channels);
    let mut chans: Vec<Vec<i64>> = vec![Vec::new(); channels];
    while pos + 2 <= bytes.len() {
        // Frames start on a sync code; anything else (tags at the end) is skipped.
        if bytes[pos] != 0xFF || bytes[pos + 1] & 0xFE != 0xF8 {
            pos += 1;
            continue;
        }
        pos = decode_frame(bytes, pos, bits, channels, &mut chans)?;
        let n = chans[0].len();
        for i in 0..n {
            for ch in &chans {
                samples.push(ch[i] as i32);
            }
        }
        if total > 0 && samples.len() as u64 >= total * channels as u64 {
            break;
        }
    }
    if total > 0 {
        samples.truncate((total as usize).saturating_mul(channels));
    }
    Ok(Stream { sample_rate, channels, bits, samples, md5, min_block, max_block })
}

/// Decodes the frame at `start` into `chans`; returns the position after it.
fn decode_frame(bytes: &[u8], start: usize, stream_bits: u32, channels: usize, chans: &mut [Vec<i64>]) -> R<usize> {
    let mut r = BitReader::new(bytes, start);
    // Sync code and blocking strategy (the frame or sample number is not needed).
    r.get(16)?;
    let bs_code = r.get(4)?;
    let sr_code = r.get(4)?;
    let assignment = r.get(4)?;
    let ss_code = r.get(3)?;
    if r.get(1)? != 0 {
        return Err(corrupt());
    }
    // Frame or sample number, coded like UTF-8.
    let first = r.get(8)?;
    let extra = (first as u8).leading_ones();
    if extra == 1 || extra > 7 {
        return Err(corrupt());
    }
    for _ in 1..extra {
        if r.get(8)? & 0xC0 != 0x80 {
            return Err(corrupt());
        }
    }
    let n = match bs_code {
        0 => return Err(corrupt()),
        1 => 192,
        2..=5 => 576 << (bs_code - 2),
        6 => r.get(8)? as usize + 1,
        7 => r.get(16)? as usize + 1,
        _ => 256 << (bs_code - 8),
    };
    match sr_code {
        12 => {
            r.get(8)?;
        }
        13 | 14 => {
            r.get(16)?;
        }
        15 => return Err(corrupt()),
        _ => {}
    }
    let bits = match ss_code {
        0 => stream_bits,
        1 => 8,
        2 => 12,
        4 => 16,
        5 => 20,
        6 => 24,
        7 => 32,
        _ => return Err(corrupt()),
    };
    let header_end = r.byte_pos();
    if crc8(&bytes[start..header_end]) != r.get(8)? as u8 {
        return Err(corrupt());
    }
    let count = if assignment <= 7 { assignment as usize + 1 } else if assignment <= 10 { 2 } else { return Err(corrupt()) };
    if count != channels {
        return Err(corrupt());
    }
    for (c, ch) in chans.iter_mut().enumerate() {
        let side = matches!((assignment, c), (8, 1) | (9, 0) | (10, 1));
        decode_subframe(&mut r, n, bits + side as u32, ch)?;
    }
    r.align();
    let end = r.byte_pos();
    if crc16(&bytes[start..end]) != r.get(16)? as u16 {
        return Err(corrupt());
    }
    match assignment {
        8 => {
            let (l, s) = chans.split_at_mut(1);
            for (a, b) in l[0].iter().zip(s[0].iter_mut()) {
                *b = a - *b;
            }
        }
        9 => {
            let (s, rr) = chans.split_at_mut(1);
            for (a, b) in s[0].iter_mut().zip(&rr[0]) {
                *a += b;
            }
        }
        10 => {
            let (m, s) = chans.split_at_mut(1);
            for (mid, side) in m[0].iter_mut().zip(s[0].iter_mut()) {
                let full = (*mid << 1) | (*side & 1);
                let (l, rr) = ((full + *side) >> 1, (full - *side) >> 1);
                *mid = l;
                *side = rr;
            }
        }
        _ => {}
    }
    Ok(end + 2)
}

fn decode_subframe(r: &mut BitReader, n: usize, bits: u32, out: &mut Vec<i64>) -> R<()> {
    out.clear();
    if r.get(1)? != 0 {
        return Err(corrupt());
    }
    let kind = r.get(6)?;
    let wasted = if r.get(1)? == 1 { r.unary()? as u32 + 1 } else { 0 };
    if wasted >= bits {
        return Err(corrupt());
    }
    let bits = bits - wasted;
    match kind {
        0 => {
            let v = r.signed(bits)?;
            out.resize(n, v);
        }
        1 => {
            for _ in 0..n {
                out.push(r.signed(bits)?);
            }
        }
        8..=12 => {
            let order = (kind - 8) as usize;
            if order > n {
                return Err(corrupt());
            }
            for _ in 0..order {
                out.push(r.signed(bits)?);
            }
            read_residual(r, n, order, out)?;
            for i in order..n {
                let p = match order {
                    0 => 0,
                    1 => out[i - 1],
                    2 => 2 * out[i - 1] - out[i - 2],
                    3 => 3 * out[i - 1] - 3 * out[i - 2] + out[i - 3],
                    _ => 4 * out[i - 1] - 6 * out[i - 2] + 4 * out[i - 3] - out[i - 4],
                };
                out[i] += p;
            }
        }
        32..=63 => {
            let order = (kind - 31) as usize;
            if order > n {
                return Err(corrupt());
            }
            for _ in 0..order {
                out.push(r.signed(bits)?);
            }
            let precision = r.get(4)? as u32 + 1;
            if precision == 16 {
                return Err(corrupt());
            }
            let shift = r.signed(5)?;
            if shift < 0 {
                return Err(unsupported());
            }
            let coefs: Vec<i64> = (0..order).map(|_| r.signed(precision)).collect::<R<_>>()?;
            read_residual(r, n, order, out)?;
            for i in order..n {
                let sum: i64 = coefs.iter().enumerate().map(|(j, &c)| c.wrapping_mul(out[i - 1 - j])).fold(0i64, i64::wrapping_add);
                out[i] += sum >> shift;
            }
        }
        _ => return Err(corrupt()),
    }
    if wasted > 0 {
        out.iter_mut().for_each(|v| *v <<= wasted);
    }
    Ok(())
}

/// Appends the `n - pred` residual values after the warm-up.
fn read_residual(r: &mut BitReader, n: usize, pred: usize, out: &mut Vec<i64>) -> R<()> {
    let method = r.get(2)?;
    let (param_bits, escape) = match method {
        0 => (4, 15),
        1 => (5, 31),
        _ => return Err(corrupt()),
    };
    let order = r.get(4)? as u32;
    let parts = 1usize << order;
    if n % parts != 0 || (n >> order) < pred {
        return Err(corrupt());
    }
    let size = n >> order;
    for p in 0..parts {
        let count = if p == 0 { size - pred } else { size };
        let k = r.get(param_bits)? as u32;
        if k == escape {
            let raw = r.get(5)? as u32;
            for _ in 0..count {
                out.push(r.signed(raw)?);
            }
        } else {
            for _ in 0..count {
                let q = r.unary()?;
                let u = (q << k) | r.get(k)?;
                out.push(((u >> 1) as i64) ^ -((u & 1) as i64));
            }
        }
    }
    Ok(())
}

/// Decodes a FLAC file into a sample: the first two channels, in [−1, 1].
pub fn decode_sample(bytes: &[u8], name: &str) -> R<Sample> {
    let s = decode(bytes)?;
    if s.channels == 0 {
        return Err(corrupt());
    }
    let scale = 1.0 / (1u64 << (s.bits - 1)) as f64;
    let frames = s.frames();
    let mut left = Vec::with_capacity(frames);
    let mut right = Vec::with_capacity(frames);
    for f in s.samples.chunks_exact(s.channels) {
        let l = (f[0] as f64 * scale) as f32;
        left.push(l);
        right.push(if s.channels > 1 { (f[1] as f64 * scale) as f32 } else { l });
    }
    Ok(Sample { name: name.to_string(), sample_rate: s.sample_rate as f32, left, right, markers: Vec::new() })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// xorshift: reproducible noise without a crate.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn range(&mut self, bits: u32) -> i32 {
            let v = (self.next() >> (64 - bits)) as i64 - (1i64 << (bits - 1));
            v as i32
        }
    }

    fn round_trip(samples: &[i32], channels: usize, bits: u32, opts: EncodeOptions) -> Vec<u8> {
        let flac = encode(samples, channels, 44_100, bits, opts);
        let s = decode(&flac).expect("decodes");
        assert_eq!((s.channels, s.bits, s.sample_rate), (channels, bits, 44_100));
        assert!(s.samples == samples, "{bits} bits, {channels} ch, block {}: samples differ", opts.block_size);
        assert_eq!(s.md5, audio_md5(samples, bits));
        assert!(s.md5_ok());
        flac
    }

    /// Stereo music-like signal: two detuned partials, a slow envelope and a little noise.
    fn music(frames: usize, bits: u32, seed: u64) -> Vec<i32> {
        let mut rng = Rng(seed);
        let amp = ((1i64 << (bits - 1)) - 1) as f64 * 0.6;
        (0..frames)
            .flat_map(|i| {
                let t = i as f64 / 44_100.0;
                let env = 0.5 + 0.5 * (t * 1.3).sin();
                let a = (t * 220.0 * std::f64::consts::TAU).sin() * 0.7 + (t * 331.0 * std::f64::consts::TAU).sin() * 0.3;
                let b = (t * 221.5 * std::f64::consts::TAU).sin() * 0.6 + (t * 440.0 * std::f64::consts::TAU).sin() * 0.4;
                let noise = (rng.range(8) as f64) / 128.0 * 0.002;
                [((a * env + noise) * amp) as i32, ((b * env - noise) * amp) as i32]
            })
            .collect()
    }

    #[test]
    fn random_and_real_signals_round_trip_bit_exact() {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        for bits in [16, 24] {
            let noise: Vec<i32> = (0..2 * 9_000).map(|_| rng.range(bits)).collect();
            let tone = music(9_000, bits, 7);
            let lo = -(1i32 << (bits - 1));
            let hi = (1i32 << (bits - 1)) - 1;
            // Full-scale square and a step: the extreme values, with mid/side wraps.
            let square: Vec<i32> = (0..2 * 3_000).map(|i| if (i / 2 / 37) % 2 == 0 { [hi, lo][i % 2] } else { [lo, hi][i % 2] }).collect();
            let silence = vec![0; 2 * 5_000];
            for block in [16, 100, 192, 576, 1_152, 4_096, 4_608, 8_191] {
                let opts = EncodeOptions { block_size: block, ..Default::default() };
                for signal in [&noise, &tone, &square, &silence] {
                    round_trip(signal, 2, bits, opts);
                }
            }
            // Mono, and odd lengths that end on a short block.
            let mono: Vec<i32> = tone.iter().step_by(2).copied().take(5_001).collect();
            round_trip(&mono, 1, bits, EncodeOptions::default());
            round_trip(&tone[..2 * 4_097], 2, bits, EncodeOptions { max_lpc_order: 0, ..Default::default() });
            round_trip(&tone[..2 * 333], 2, bits, EncodeOptions { stereo: false, max_lpc_order: 8, ..Default::default() });
        }
        // Other depths and channel counts.
        let six: Vec<i32> = (0..6 * 2_000).map(|_| rng.range(12)).collect();
        round_trip(&six, 6, 12, EncodeOptions::default());
        let eight: Vec<i32> = (0..3_000).map(|i| ((i as f64 * 0.05).sin() * 100.0) as i32).collect();
        round_trip(&eight, 1, 8, EncodeOptions::default());
        // Empty stream.
        let s = decode(&encode(&[], 2, 48_000, 16, EncodeOptions::default())).unwrap();
        assert!(s.samples.is_empty());
    }

    #[test]
    fn real_audio_compresses_and_noise_barely_grows() {
        let tone = music(44_100, 16, 3);
        let raw = tone.len() * 2;
        let flac = round_trip(&tone, 2, 16, EncodeOptions::default());
        let ratio = flac.len() as f64 / raw as f64;
        assert!(ratio < 0.6, "tonal audio: {ratio:.3}");
        // LPC beats fixed prediction alone on a tonal signal.
        let fixed = encode(&tone, 2, 44_100, 16, EncodeOptions { max_lpc_order: 0, ..Default::default() });
        assert!(flac.len() < fixed.len(), "{} vs {}", flac.len(), fixed.len());
        // White noise cannot be predicted: verbatim at worst, with frame headers.
        let mut rng = Rng(5);
        let noise: Vec<i32> = (0..2 * 44_100).map(|_| rng.range(16)).collect();
        let flac = round_trip(&noise, 2, 16, EncodeOptions::default());
        assert!(flac.len() as f64 / raw as f64 <= 1.01, "noise: {}", flac.len() as f64 / raw as f64);
        // Silence is nearly free.
        let flac = round_trip(&vec![0; 2 * 44_100], 2, 16, EncodeOptions::default());
        assert!(flac.len() < 1_000, "{}", flac.len());
        // Identical channels: the side channel is silent.
        let dual: Vec<i32> = tone.chunks(2).flat_map(|f| [f[0], f[0]]).collect();
        let stereo = round_trip(&dual, 2, 16, EncodeOptions::default());
        let mono: Vec<i32> = tone.chunks(2).map(|f| f[0]).collect();
        let single = round_trip(&mono, 1, 16, EncodeOptions::default());
        assert!(stereo.len() < single.len() + single.len() / 20, "{} vs {}", stereo.len(), single.len());
    }

    #[test]
    fn streaminfo_describes_the_stream() {
        let tone = music(10_000, 24, 1);
        let flac = encode(&tone, 2, 96_000, 24, EncodeOptions { block_size: 1_152, ..Default::default() });
        assert_eq!(&flac[..4], b"fLaC");
        assert_eq!(flac[4], 0x80, "STREAMINFO is the last metadata block");
        let s = decode(&flac).unwrap();
        assert_eq!((s.min_block, s.max_block, s.sample_rate, s.frames()), (1_152, 1_152, 96_000, 10_000));
        assert!(is_flac(&flac) && !is_flac(b"RIFF"));
        // An ID3v2 tag in front is skipped.
        let mut tagged = b"ID3\x04\x00\x00\x00\x00\x00\x05abcde".to_vec();
        tagged.extend_from_slice(&flac);
        assert!(is_flac(&tagged));
        assert_eq!(decode(&tagged).unwrap().samples, tone);
    }

    #[test]
    fn corruption_is_caught() {
        let tone = music(5_000, 16, 2);
        let flac = encode(&tone, 2, 44_100, 16, EncodeOptions::default());
        // A flipped bit in the audio: CRC-16 refuses the frame.
        let mut bad = flac.clone();
        let at = bad.len() - 100;
        bad[at] ^= 0x10;
        assert!(decode(&bad).is_err());
        // A wrong MD5 decodes but does not verify.
        let mut wrong = flac.clone();
        wrong[8 + 18] ^= 1;
        let s = decode(&wrong).unwrap();
        assert!(!s.md5_ok());
        assert!(decode(b"fLaC").is_err());
        assert!(decode(b"RIFF....WAVE").is_err());
    }

    #[test]
    fn decoder_reads_what_other_encoders_write() {
        // Hand-built frame: wasted bits, an escaped partition and a second-order
        // fixed subframe, left/side stereo, block size from an 8-bit field.
        let n = 10usize;
        let left: Vec<i64> = (0..n as i64).map(|i| i * 8).collect();
        let right: Vec<i64> = (0..n as i64).map(|i| -i * 3 + 5).collect();
        let mut w = BitWriter::default();
        w.put(0xFFF8, 16);
        w.put(6, 4);
        w.put(0, 4);
        w.put(8, 4);
        w.put(4, 3);
        w.put(0, 1);
        put_utf8(&mut w, 0);
        w.put(n as u64 - 1, 8);
        let crc = crc8(&w.bytes);
        w.put(crc as u64, 8);
        // Left: verbatim with 3 wasted bits (values are multiples of 8).
        w.put(0, 1);
        w.put(1, 6);
        w.put(1, 1);
        w.unary(2);
        for &v in &left {
            w.signed(v >> 3, 13);
        }
        // Side (17 bits): fixed order 2, an escaped partition of raw 6-bit values.
        let side: Vec<i64> = left.iter().zip(&right).map(|(a, b)| a - b).collect();
        w.put(0, 1);
        w.put(8 | 2, 6);
        w.put(0, 1);
        w.signed(side[0], 17);
        w.signed(side[1], 17);
        w.put(0, 2);
        w.put(0, 4);
        w.put(15, 4);
        w.put(6, 5);
        for r in fixed_residual(&side, 2) {
            w.signed(r, 6);
        }
        w.align();
        let crc = crc16(&w.bytes);
        w.put(crc as u64, 16);
        let mut file = encode(&[], 2, 44_100, 16, EncodeOptions::default());
        file.extend_from_slice(&w.bytes);
        // Total samples unknown (0): the frame decides.
        let s = decode(&file).unwrap();
        let want: Vec<i32> = (0..n).flat_map(|i| [left[i] as i32, right[i] as i32]).collect();
        assert_eq!(s.samples, want);
    }

    #[test]
    fn frame_numbers_use_the_utf8_coding() {
        for v in [0u64, 0x7f, 0x80, 0x7ff, 0x800, 0xffff, 0x10000, 0x1f_ffff, 0x20_0000, 0x3ff_ffff, 0x400_0000, 0xf_ffff_ffff] {
            let mut w = BitWriter::default();
            put_utf8(&mut w, v);
            let lead = w.bytes[0].leading_ones() as usize;
            assert_eq!(w.bytes.len(), if v < 0x80 { 1 } else { lead }, "{v:#x}");
            let mut r = BitReader::new(&w.bytes, 0);
            let first = r.get(8).unwrap();
            let mut got = if lead == 0 { first } else { first & (0x7f >> lead) };
            for _ in 1..w.bytes.len() {
                got = (got << 6) | (r.get(8).unwrap() & 0x3f);
            }
            assert_eq!(got, v);
        }
    }

    #[test]
    fn decoded_samples_are_scaled_to_unity() {
        let flac = encode(&[i16::MIN as i32, i16::MAX as i32, 0, 16_384], 2, 48_000, 16, EncodeOptions::default());
        let s = decode_sample(&flac, "x").unwrap();
        assert_eq!(s.left, [-1.0, 0.0]);
        assert_eq!(s.right[1], 0.5);
        assert!((s.right[0] - 1.0).abs() < 1e-4);
        assert_eq!(s.sample_rate, 48_000.0);
    }
}
