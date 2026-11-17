//! Vorbis I decoder, written from the Xiph.org specification: the three
//! headers, codebooks (Huffman trees and VQ lookup types 1 and 2), floors 0
//! and 1, residues 0, 1 and 2, mappings with channel coupling, windowing, the
//! IMDCT (through an FFT) and overlap-add. The Ogg layer ([`super::ogg`])
//! feeds it packets.

use std::f64::consts::PI;

/// The packet ended before the decoder was done with it.
#[derive(Debug)]
pub struct Eop;

type R<T> = Result<T, Eop>;

/// Bit reader of the Vorbis packets: least significant bit first.
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Bits { data, pos: 0 }
    }

    fn read(&mut self, n: u32) -> R<u32> {
        if n == 0 {
            return Ok(0);
        }
        if self.pos + n as usize > self.data.len() * 8 {
            self.pos = self.data.len() * 8;
            return Err(Eop);
        }
        let byte = self.pos / 8;
        let mut word = 0u64;
        for k in 0..5 {
            word |= (*self.data.get(byte + k).unwrap_or(&0) as u64) << (8 * k);
        }
        let v = (word >> (self.pos % 8)) & ((1u64 << n) - 1);
        self.pos += n as usize;
        Ok(v as u32)
    }

    fn bit(&mut self) -> R<bool> {
        Ok(self.read(1)? == 1)
    }

    fn left(&self) -> usize {
        self.data.len() * 8 - self.pos
    }
}

/// Bits needed to write `v`.
fn ilog(v: u32) -> u32 {
    32 - v.leading_zeros()
}

/// The 32-bit float format of the codebooks.
fn float32(x: u32) -> f32 {
    let mantissa = (x & 0x1F_FFFF) as f64;
    let exponent = ((x >> 21) & 0x3FF) as i32;
    let m = if x & 0x8000_0000 != 0 { -mantissa } else { mantissa };
    (m * 2f64.powi(exponent - 788)) as f32
}

/// Largest `r` with `r^dims <= entries`.
fn lookup1_values(entries: u32, dims: u32) -> u32 {
    let fits = |r: u64| {
        let mut p = 1u64;
        for _ in 0..dims {
            p = match p.checked_mul(r) {
                Some(p) if p <= entries as u64 => p,
                _ => return false,
            };
        }
        true
    };
    let mut r = (entries as f64).powf(1.0 / dims as f64).floor() as u64;
    while r > 0 && !fits(r) {
        r -= 1;
    }
    while fits(r + 1) {
        r += 1;
    }
    r as u32
}

fn bad(what: &str) -> String {
    format!("Vorbis: {what}")
}

/// A codebook: a Huffman tree over its entries and, for VQ books, the
/// vectors of each entry.
struct Codebook {
    dims: usize,
    /// Tree nodes: children, `LEAF | entry` for a leaf, 0 for none.
    tree: Vec<[u32; 2]>,
    /// The single used entry of a one-entry book (and its length).
    single: Option<(u32, u32)>,
    /// `dims` values per entry, for books with a lookup table.
    vq: Option<Vec<f32>>,
}

const LEAF: u32 = 1 << 31;

/// Decoding tree and single entry of a codebook (see [`Codebook`]).
type Tree = (Vec<[u32; 2]>, Option<(u32, u32)>);

impl Codebook {
    fn parse(b: &mut Bits) -> Result<Codebook, String> {
        let err = |_| bad("truncated codebook");
        if b.read(24).map_err(err)? != 0x56_4342 {
            return Err(bad("codebook sync"));
        }
        let dims = b.read(16).map_err(err)?;
        let entries = b.read(24).map_err(err)?;
        if entries as usize > b.left() {
            return Err(bad("codebook size"));
        }
        let mut lengths = vec![0u8; entries as usize];
        if !b.bit().map_err(err)? {
            let sparse = b.bit().map_err(err)?;
            for l in lengths.iter_mut() {
                if !sparse || b.bit().map_err(err)? {
                    *l = b.read(5).map_err(err)? as u8 + 1;
                }
            }
        } else {
            let mut len = b.read(5).map_err(err)? + 1;
            let mut at = 0u32;
            while at < entries {
                let count = b.read(ilog(entries - at)).map_err(err)?;
                if at + count > entries || len > 32 {
                    return Err(bad("codebook lengths"));
                }
                lengths[at as usize..(at + count) as usize].fill(len as u8);
                at += count;
                len += 1;
            }
        }
        let lookup = b.read(4).map_err(err)?;
        let vq = match lookup {
            0 => None,
            1 | 2 => {
                let min = float32(b.read(32).map_err(err)?);
                let delta = float32(b.read(32).map_err(err)?);
                let bits = b.read(4).map_err(err)? + 1;
                let sequence = b.bit().map_err(err)?;
                let count = if lookup == 1 { lookup1_values(entries, dims) as u64 } else { entries as u64 * dims as u64 };
                if dims == 0 || count * bits as u64 > b.left() as u64 || entries as u64 * dims as u64 > 1 << 22 {
                    return Err(bad("codebook lookup"));
                }
                let mults: Vec<u32> = (0..count).map(|_| b.read(bits)).collect::<R<_>>().map_err(err)?;
                let (dims, count) = (dims as usize, count as usize);
                let mut values = Vec::with_capacity(entries as usize * dims);
                for e in 0..entries as usize {
                    let mut last = 0f32;
                    let mut divisor = 1usize;
                    for k in 0..dims {
                        let index = if lookup == 1 { (e / divisor) % count } else { e * dims + k };
                        let v = mults[index] as f32 * delta + min + last;
                        if sequence {
                            last = v;
                        }
                        values.push(v);
                        divisor = divisor.saturating_mul(count);
                    }
                }
                Some(values)
            }
            _ => return Err(bad("codebook lookup type")),
        };
        let (tree, single) = Self::build_tree(&lengths)?;
        Ok(Codebook { dims: dims as usize, tree, single, vq })
    }

    /// Canonical codewords in entry order (the specification's "lowest
    /// available" rule), then the decoding tree.
    fn build_tree(lengths: &[u8]) -> Result<Tree, String> {
        let used: Vec<usize> = (0..lengths.len()).filter(|&e| lengths[e] > 0).collect();
        if used.len() == 1 {
            let e = used[0];
            return Ok((Vec::new(), Some((e as u32, lengths[e] as u32))));
        }
        let mut marker = [0u32; 33];
        let mut tree = vec![[0u32; 2]];
        for &e in &used {
            let len = lengths[e] as usize;
            let code = marker[len];
            if len < 32 && code >> len != 0 {
                return Err(bad("overspecified codebook"));
            }
            for j in (1..=len).rev() {
                if marker[j] & 1 != 0 {
                    marker[j] = if j == 1 { marker[1] + 1 } else { marker[j - 1] << 1 };
                    break;
                }
                marker[j] += 1;
            }
            let mut entry = code;
            for j in len + 1..33 {
                if marker[j] >> 1 == entry {
                    entry = marker[j];
                    marker[j] = marker[j - 1] << 1;
                } else {
                    break;
                }
            }
            // Insert the codeword, most significant bit first.
            let mut node = 0;
            for k in (0..len).rev() {
                let bit = ((code >> k) & 1) as usize;
                let child = tree[node][bit];
                if k == 0 {
                    if child != 0 {
                        return Err(bad("overspecified codebook"));
                    }
                    tree[node][bit] = LEAF | e as u32;
                } else if child == 0 {
                    tree.push([0, 0]);
                    let at = tree.len() as u32 - 1;
                    tree[node][bit] = at;
                    node = at as usize;
                } else if child & LEAF != 0 {
                    return Err(bad("overspecified codebook"));
                } else {
                    node = child as usize;
                }
            }
        }
        Ok((tree, None))
    }

    fn decode(&self, b: &mut Bits) -> R<usize> {
        if let Some((entry, len)) = self.single {
            b.read(len)?;
            return Ok(entry as usize);
        }
        let mut node = 0usize;
        loop {
            let next = *self.tree.get(node).ok_or(Eop)?.get(b.read(1)? as usize).ok_or(Eop)?;
            if next == 0 {
                // A codeword the tree does not have.
                return Err(Eop);
            }
            if next & LEAF != 0 {
                return Ok((next & !LEAF) as usize);
            }
            node = next as usize;
        }
    }

    fn decode_vq(&self, b: &mut Bits) -> R<&[f32]> {
        let entry = self.decode(b)?;
        let vq = self.vq.as_ref().ok_or(Eop)?;
        vq.get(entry * self.dims..(entry + 1) * self.dims).ok_or(Eop)
    }
}

struct Floor0 {
    order: usize,
    rate: u32,
    bark_map_size: u32,
    amplitude_bits: u32,
    amplitude_offset: u32,
    books: Vec<usize>,
}

struct Floor1 {
    partition_class: Vec<usize>,
    class_dims: Vec<usize>,
    class_subclasses: Vec<u32>,
    class_masterbook: Vec<usize>,
    /// Per class, per subclass: a book or `None`.
    subclass_books: Vec<Vec<Option<usize>>>,
    multiplier: u32,
    xs: Vec<u32>,
    /// Indices of `xs` in increasing X order.
    sorted: Vec<usize>,
    low: Vec<usize>,
    high: Vec<usize>,
}

enum Floor {
    Zero(Floor0),
    One(Floor1),
}

/// What a floor decoded for one channel.
enum FloorData {
    Zero { amplitude: u32, coefficients: Vec<f32> },
    One(Vec<i32>),
}

impl Floor {
    fn parse(b: &mut Bits, books: &[Codebook]) -> Result<Floor, String> {
        let err = |_| bad("truncated floor");
        let book = |i: u32| if (i as usize) < books.len() { Ok(i as usize) } else { Err(bad("floor book")) };
        match b.read(16).map_err(err)? {
            0 => {
                let order = b.read(8).map_err(err)? as usize;
                let rate = b.read(16).map_err(err)?;
                let bark_map_size = b.read(16).map_err(err)?;
                let amplitude_bits = b.read(6).map_err(err)?;
                let amplitude_offset = b.read(8).map_err(err)?;
                let count = b.read(4).map_err(err)? + 1;
                let books_: Vec<usize> = (0..count).map(|_| b.read(8).map_err(err).and_then(book)).collect::<Result<_, _>>()?;
                if order == 0 || rate == 0 || bark_map_size == 0 || books_.iter().any(|&i| books[i].dims == 0 || books[i].vq.is_none()) {
                    return Err(bad("floor 0"));
                }
                Ok(Floor::Zero(Floor0 { order, rate, bark_map_size, amplitude_bits, amplitude_offset, books: books_ }))
            }
            1 => {
                let partitions = b.read(5).map_err(err)? as usize;
                let partition_class: Vec<usize> = (0..partitions).map(|_| b.read(4).map(|v| v as usize)).collect::<R<_>>().map_err(err)?;
                let classes = partition_class.iter().max().map_or(0, |&m| m + 1);
                let mut class_dims = Vec::new();
                let mut class_subclasses = Vec::new();
                let mut class_masterbook = Vec::new();
                let mut subclass_books = Vec::new();
                for _ in 0..classes {
                    class_dims.push(b.read(3).map_err(err)? as usize + 1);
                    let sub = b.read(2).map_err(err)?;
                    class_subclasses.push(sub);
                    class_masterbook.push(if sub > 0 { book(b.read(8).map_err(err)?)? } else { 0 });
                    let mut list = Vec::new();
                    for _ in 0..1 << sub {
                        let v = b.read(8).map_err(err)?;
                        list.push(if v == 0 { None } else { Some(book(v - 1)?) });
                    }
                    subclass_books.push(list);
                }
                let multiplier = b.read(2).map_err(err)? + 1;
                let range_bits = b.read(4).map_err(err)?;
                let mut xs = vec![0, 1 << range_bits];
                for &class in &partition_class {
                    for _ in 0..class_dims[class] {
                        xs.push(b.read(range_bits).map_err(err)?);
                    }
                }
                if xs.len() > 65 {
                    return Err(bad("floor 1 points"));
                }
                let mut sorted: Vec<usize> = (0..xs.len()).collect();
                sorted.sort_by_key(|&i| xs[i]);
                if sorted.windows(2).any(|w| xs[w[0]] == xs[w[1]]) {
                    return Err(bad("floor 1 points"));
                }
                // Neighbors: the closest earlier points below and above in X.
                let mut low = vec![0; xs.len()];
                let mut high = vec![0; xs.len()];
                for i in 2..xs.len() {
                    let (mut lo, mut hi) = (0, 1);
                    for j in 0..i {
                        if xs[j] < xs[i] && xs[j] > xs[lo] {
                            lo = j;
                        }
                        if xs[j] > xs[i] && xs[j] < xs[hi] {
                            hi = j;
                        }
                    }
                    low[i] = lo;
                    high[i] = hi;
                }
                Ok(Floor::One(Floor1 { partition_class, class_dims, class_subclasses, class_masterbook, subclass_books, multiplier, xs, sorted, low, high }))
            }
            _ => Err(bad("floor type")),
        }
    }

    /// Reads the floor of one channel; `None` when it is unused.
    fn decode(&self, b: &mut Bits, books: &[Codebook]) -> R<Option<FloorData>> {
        match self {
            Floor::Zero(f) => {
                let amplitude = b.read(f.amplitude_bits)?;
                if amplitude == 0 {
                    return Ok(None);
                }
                let index = b.read(ilog(f.books.len() as u32))? as usize;
                let book = &books[*f.books.get(index).ok_or(Eop)?];
                let mut coefficients = Vec::with_capacity(f.order + book.dims);
                let mut last = 0f32;
                while coefficients.len() < f.order {
                    let v = book.decode_vq(b)?;
                    coefficients.extend(v.iter().map(|&x| x + last));
                    last = *coefficients.last().unwrap_or(&0.0);
                }
                coefficients.truncate(f.order);
                Ok(Some(FloorData::Zero { amplitude, coefficients }))
            }
            Floor::One(f) => {
                if !b.bit()? {
                    return Ok(None);
                }
                let range = [256, 128, 86, 64][f.multiplier as usize - 1];
                let bits = ilog(range - 1);
                let mut y = vec![0i32; f.xs.len()];
                y[0] = b.read(bits)? as i32;
                y[1] = b.read(bits)? as i32;
                let mut at = 2;
                for &class in &f.partition_class {
                    let dims = f.class_dims[class];
                    let sub = f.class_subclasses[class];
                    let mask = (1 << sub) - 1;
                    let mut cval = if sub > 0 { books[f.class_masterbook[class]].decode(b)? } else { 0 };
                    for j in 0..dims {
                        if let Some(book) = f.subclass_books[class][cval & mask] {
                            y[at + j] = books[book].decode(b)? as i32;
                        }
                        cval >>= sub;
                    }
                    at += dims;
                }
                Ok(Some(FloorData::One(y)))
            }
        }
    }

    /// The floor curve of `n` values (half a block) from what was decoded.
    fn curve(&self, data: &FloorData, n: usize, out: &mut [f32]) {
        match (self, data) {
            (Floor::Zero(f), FloorData::Zero { amplitude, coefficients }) => f.curve(*amplitude, coefficients, n, out),
            (Floor::One(f), FloorData::One(y)) => f.curve(y, n, out),
            _ => out.fill(0.0),
        }
    }
}

impl Floor0 {
    fn curve(&self, amplitude: u32, coefficients: &[f32], n: usize, out: &mut [f32]) {
        let bark = |x: f64| 13.1 * (0.00074 * x).atan() + 2.24 * (0.000_000_018_5 * x * x).atan() + 0.0001 * x;
        let rate = self.rate as f64;
        let size = self.bark_map_size as f64;
        let map = |i: usize| -> i64 {
            let v = (bark(rate * i as f64 / (2.0 * n as f64)) * size / bark(0.5 * rate)).floor() as i64;
            v.min(self.bark_map_size as i64 - 1)
        };
        let cos: Vec<f64> = coefficients.iter().map(|&c| (c as f64).cos()).collect();
        let max_amp = ((1u64 << self.amplitude_bits) - 1).max(1) as f64;
        let mut i = 0;
        while i < n {
            let m = map(i);
            let omega = PI * m as f64 / size;
            let w = omega.cos();
            let (mut p, mut q);
            if self.order % 2 == 1 {
                p = 1.0 - w * w;
                q = 0.25;
                for (j, c) in cos.iter().enumerate() {
                    if j % 2 == 1 {
                        p *= 4.0 * (c - w).powi(2);
                    } else {
                        q *= 4.0 * (c - w).powi(2);
                    }
                }
            } else {
                p = (1.0 - w) / 2.0;
                q = (1.0 + w) / 2.0;
                for (j, c) in cos.iter().enumerate() {
                    if j % 2 == 1 {
                        p *= 4.0 * (c - w).powi(2);
                    } else {
                        q *= 4.0 * (c - w).powi(2);
                    }
                }
            }
            let offset = self.amplitude_offset as f64;
            let value = (0.115_129_25 * (amplitude as f64 * offset / max_amp / (p + q).sqrt() - offset)).exp() as f32;
            while i < n && map(i) == m {
                out[i] = value;
                i += 1;
            }
        }
    }
}

impl Floor1 {
    fn curve(&self, y: &[i32], n: usize, out: &mut [f32]) {
        let range = [256, 128, 86, 64][self.multiplier as usize - 1];
        let count = self.xs.len();
        let mut used = vec![false; count];
        let mut fy = vec![0i32; count];
        used[0] = true;
        used[1] = true;
        fy[0] = y[0];
        fy[1] = y[1];
        for i in 2..count {
            let (lo, hi) = (self.low[i], self.high[i]);
            let predicted = render_point(self.xs[lo] as i32, fy[lo], self.xs[hi] as i32, fy[hi], self.xs[i] as i32);
            let val = y[i];
            let high_room = range - predicted;
            let low_room = predicted;
            let room = 2 * high_room.min(low_room);
            if val != 0 {
                used[lo] = true;
                used[hi] = true;
                used[i] = true;
                fy[i] = if val >= room {
                    if high_room > low_room { val - low_room + predicted } else { predicted - val + high_room - 1 }
                } else if val % 2 == 1 {
                    predicted - (val + 1) / 2
                } else {
                    predicted + val / 2
                };
            } else {
                fy[i] = predicted;
            }
        }
        let m = self.multiplier as i32;
        let (mut lx, mut ly) = (0usize, fy[self.sorted[0]] * m);
        let (mut hx, mut hy) = (0usize, 0i32);
        for &i in &self.sorted[1..] {
            if used[i] {
                hy = fy[i] * m;
                hx = self.xs[i] as usize;
                render_line(lx, ly, hx, hy, n, out);
                lx = hx;
                ly = hy;
            }
        }
        if hx < n {
            render_line(hx, hy, n, hy, n, out);
        }
    }
}

fn render_point(x0: i32, y0: i32, x1: i32, y1: i32, x: i32) -> i32 {
    let dy = y1 - y0;
    let adx = x1 - x0;
    if adx == 0 {
        return y0;
    }
    let off = dy.abs() * (x - x0) / adx;
    if dy < 0 { y0 - off } else { y0 + off }
}

/// The integer line of the specification, as floor values from `x0` to
/// `x1` (excluded), clipped to `n`.
fn render_line(x0: usize, y0: i32, x1: usize, y1: i32, n: usize, out: &mut [f32]) {
    if x1 <= x0 {
        return;
    }
    let value = |y: i32| INVERSE_DB[y.clamp(0, 255) as usize];
    let dy = y1 - y0;
    let adx = (x1 - x0) as i32;
    let base = dy / adx;
    let sy = if dy < 0 { base - 1 } else { base + 1 };
    let ady = dy.abs() - base.abs() * adx;
    let (mut y, mut err) = (y0, 0);
    if x0 < n {
        out[x0] = value(y);
    }
    for o in out.iter_mut().take(x1.min(n)).skip(x0 + 1) {
        err += ady;
        if err >= adx {
            err -= adx;
            y += sy;
        } else {
            y += base;
        }
        *o = value(y);
    }
}

struct Residue {
    kind: u32,
    begin: usize,
    end: usize,
    partition_size: usize,
    classifications: usize,
    classbook: usize,
    /// Per classification, per pass: a book or `None`.
    books: Vec<[Option<usize>; 8]>,
}

impl Residue {
    fn parse(b: &mut Bits, books: &[Codebook]) -> Result<Residue, String> {
        let err = |_| bad("truncated residue");
        let kind = b.read(16).map_err(err)?;
        if kind > 2 {
            return Err(bad("residue type"));
        }
        let begin = b.read(24).map_err(err)? as usize;
        let end = b.read(24).map_err(err)? as usize;
        let partition_size = b.read(24).map_err(err)? as usize + 1;
        let classifications = b.read(6).map_err(err)? as usize + 1;
        let classbook = b.read(8).map_err(err)? as usize;
        if books.get(classbook).is_none_or(|c| c.dims == 0) {
            return Err(bad("residue classbook"));
        }
        let mut cascade = Vec::with_capacity(classifications);
        for _ in 0..classifications {
            let low = b.read(3).map_err(err)?;
            let high = if b.bit().map_err(err)? { b.read(5).map_err(err)? } else { 0 };
            cascade.push(high << 3 | low);
        }
        let mut list = Vec::with_capacity(classifications);
        for c in cascade {
            let mut passes = [None; 8];
            for (k, p) in passes.iter_mut().enumerate() {
                if c & (1 << k) != 0 {
                    let book = b.read(8).map_err(err)? as usize;
                    if books.get(book).is_none_or(|b| b.dims == 0 || b.vq.is_none()) {
                        return Err(bad("residue book"));
                    }
                    *p = Some(book);
                }
            }
            list.push(passes);
        }
        Ok(Residue { kind, begin, end, partition_size, classifications, classbook, books: list })
    }

    /// Decodes into `vectors` (half a block each); channels flagged in
    /// `skip` are left alone.
    fn decode(&self, b: &mut Bits, books: &[Codebook], vectors: &mut [Vec<f32>], skip: &[bool], half: usize) -> R<()> {
        if self.kind == 2 {
            if skip.iter().all(|&s| s) {
                return Ok(());
            }
            let ch = vectors.len();
            let mut interleaved = vec![vec![0f32; half * ch]];
            let result = self.decode_format(b, books, &mut interleaved, &[false], half * ch, 1);
            for (i, &v) in interleaved[0].iter().enumerate() {
                vectors[i % ch][i / ch] += v;
            }
            return result;
        }
        self.decode_format(b, books, vectors, skip, half, self.kind)
    }

    fn decode_format(&self, b: &mut Bits, books: &[Codebook], vectors: &mut [Vec<f32>], skip: &[bool], size: usize, kind: u32) -> R<()> {
        let begin = self.begin.min(size);
        let end = self.end.min(size);
        if end <= begin {
            return Ok(());
        }
        let classbook = &books[self.classbook];
        let per_word = classbook.dims;
        let partitions = (end - begin) / self.partition_size;
        let mut classes = vec![vec![0usize; partitions + per_word]; vectors.len()];
        for pass in 0..8 {
            let mut p = 0;
            while p < partitions {
                if pass == 0 {
                    for (j, class) in classes.iter_mut().enumerate() {
                        if skip[j] {
                            continue;
                        }
                        let mut word = classbook.decode(b)?;
                        for i in (0..per_word).rev() {
                            class[p + i] = word % self.classifications;
                            word /= self.classifications;
                        }
                    }
                }
                for _ in 0..per_word {
                    if p >= partitions {
                        break;
                    }
                    for (j, v) in vectors.iter_mut().enumerate() {
                        if skip[j] {
                            continue;
                        }
                        let Some(book) = self.books[classes[j][p]][pass] else { continue };
                        let book = &books[book];
                        let at = begin + p * self.partition_size;
                        let n = self.partition_size;
                        if kind == 0 {
                            let step = n / book.dims;
                            for i in 0..step {
                                let entry = book.decode_vq(b)?;
                                for (k, &e) in entry.iter().enumerate() {
                                    v[at + i + k * step] += e;
                                }
                            }
                        } else {
                            let mut i = 0;
                            while i < n {
                                let entry = book.decode_vq(b)?;
                                for &e in entry {
                                    if i < n {
                                        v[at + i] += e;
                                    }
                                    i += 1;
                                }
                            }
                        }
                    }
                    p += 1;
                }
            }
        }
        Ok(())
    }
}

struct Mapping {
    coupling: Vec<(usize, usize)>,
    mux: Vec<usize>,
    submaps: Vec<(usize, usize)>,
}

struct Mode {
    long: bool,
    mapping: usize,
}

/// IMDCT of one block size, through a quarter-size complex FFT.
struct Imdct {
    n: usize,
    twiddle: Vec<(f64, f64)>,
    fft: Vec<(f64, f64)>,
    reverse: Vec<usize>,
}

impl Imdct {
    fn new(n: usize) -> Self {
        let m = n / 2;
        let l = n / 4;
        let twiddle = (0..l).map(|k| {
            let a = -PI * (k as f64 + 0.125) / m as f64;
            (a.cos(), a.sin())
        });
        let fft = (0..l / 2).map(|k| {
            let a = -2.0 * PI * k as f64 / l as f64;
            (a.cos(), a.sin())
        });
        let bits = l.trailing_zeros();
        let reverse = (0..l).map(|i| if bits == 0 { 0 } else { i.reverse_bits() >> (usize::BITS - bits) }).collect();
        Imdct { n, twiddle: twiddle.collect(), fft: fft.collect(), reverse }
    }

    /// `y[i] = Σ x[k] · cos(2π/n · (i + 1/2 + n/4)(k + 1/2))`: n/2 values in,
    /// n out.
    fn run(&self, x: &[f32], y: &mut [f32]) {
        let (n, m, l) = (self.n, self.n / 2, self.n / 4);
        // DCT-IV of size m from an l-point FFT.
        let mut re = vec![0f64; l];
        let mut im = vec![0f64; l];
        for k in 0..l {
            let (a, b) = (x[2 * k] as f64, x[m - 1 - 2 * k] as f64);
            let (c, s) = self.twiddle[k];
            let r = self.reverse[k];
            re[r] = a * c - b * s;
            im[r] = a * s + b * c;
        }
        let mut size = 2;
        while size <= l {
            let half = size / 2;
            let step = l / size;
            for start in (0..l).step_by(size) {
                for j in 0..half {
                    let (c, s) = self.fft[j * step];
                    let (p, q) = (start + j, start + j + half);
                    let tr = re[q] * c - im[q] * s;
                    let ti = re[q] * s + im[q] * c;
                    re[q] = re[p] - tr;
                    im[q] = im[p] - ti;
                    re[p] += tr;
                    im[p] += ti;
                }
            }
            size *= 2;
        }
        let mut v = vec![0f64; m];
        for k in 0..l {
            let (c, s) = self.twiddle[k];
            v[2 * k] = re[k] * c - im[k] * s;
            v[m - 1 - 2 * k] = -(re[k] * s + im[k] * c);
        }
        // Unfold the DCT-IV into the IMDCT.
        for (i, out) in y.iter_mut().enumerate().take(n) {
            let j = i + m / 2;
            *out = if j < m {
                v[j]
            } else if j < 2 * m {
                -v[2 * m - 1 - j]
            } else {
                -v[j - 2 * m]
            } as f32;
        }
    }
}

/// The rising half of the Vorbis window over `n` samples.
fn slope(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| {
            let s = ((i as f64 + 0.5) / n as f64 * PI / 2.0).sin();
            (PI / 2.0 * s * s).sin() as f32
        })
        .collect()
}

/// Vorbis stream decoder.
pub struct Vorbis {
    pub channels: usize,
    pub sample_rate: u32,
    block: [usize; 2],
    books: Vec<Codebook>,
    floors: Vec<Floor>,
    residues: Vec<Residue>,
    mappings: Vec<Mapping>,
    modes: Vec<Mode>,
    imdct: [Imdct; 2],
    slopes: [Vec<f32>; 2],
    /// The previous block, windowed, per channel (empty before the first).
    prev: Vec<Vec<f32>>,
}

/// Fields of the identification header.
pub struct Identification {
    pub channels: usize,
    pub sample_rate: u32,
    block: [usize; 2],
}

impl Identification {
    pub fn parse(p: &[u8]) -> Result<Identification, String> {
        if p.len() < 30 || &p[..7] != b"\x01vorbis" {
            return Err(bad("identification header"));
        }
        let le = |at: usize| u32::from_le_bytes([p[at], p[at + 1], p[at + 2], p[at + 3]]);
        let channels = p[11] as usize;
        let sample_rate = le(12);
        let block = [1usize << (p[28] & 15), 1usize << (p[28] >> 4)];
        if le(7) != 0 || channels == 0 || sample_rate == 0 || block[0] < 64 || block[1] > 8192 || block[0] > block[1] {
            return Err(bad("identification header"));
        }
        Ok(Identification { channels, sample_rate, block })
    }
}

impl Vorbis {
    /// A decoder from the identification and setup headers.
    pub fn new(ident: &[u8], setup: &[u8]) -> Result<Vorbis, String> {
        let id = Identification::parse(ident)?;
        if setup.len() < 7 || &setup[..7] != b"\x05vorbis" {
            return Err(bad("setup header"));
        }
        let mut b = Bits::new(&setup[7..]);
        let err = |_| bad("truncated setup header");
        let count = b.read(8).map_err(err)? + 1;
        let books = (0..count).map(|_| Codebook::parse(&mut b)).collect::<Result<Vec<_>, _>>()?;
        let times = b.read(6).map_err(err)? + 1;
        for _ in 0..times {
            if b.read(16).map_err(err)? != 0 {
                return Err(bad("time domain transform"));
            }
        }
        let count = b.read(6).map_err(err)? + 1;
        let floors = (0..count).map(|_| Floor::parse(&mut b, &books)).collect::<Result<Vec<_>, _>>()?;
        let count = b.read(6).map_err(err)? + 1;
        let residues = (0..count).map(|_| Residue::parse(&mut b, &books)).collect::<Result<Vec<_>, _>>()?;
        let count = b.read(6).map_err(err)? + 1;
        let mut mappings = Vec::new();
        let ch_bits = ilog(id.channels as u32 - 1);
        for _ in 0..count {
            if b.read(16).map_err(err)? != 0 {
                return Err(bad("mapping type"));
            }
            let submaps = if b.bit().map_err(err)? { b.read(4).map_err(err)? as usize + 1 } else { 1 };
            let mut coupling = Vec::new();
            if b.bit().map_err(err)? {
                for _ in 0..b.read(8).map_err(err)? + 1 {
                    let magnitude = b.read(ch_bits).map_err(err)? as usize;
                    let angle = b.read(ch_bits).map_err(err)? as usize;
                    if magnitude == angle || magnitude >= id.channels || angle >= id.channels {
                        return Err(bad("channel coupling"));
                    }
                    coupling.push((magnitude, angle));
                }
            }
            if b.read(2).map_err(err)? != 0 {
                return Err(bad("mapping reserved bits"));
            }
            let mut mux = vec![0; id.channels];
            if submaps > 1 {
                for m in mux.iter_mut() {
                    *m = b.read(4).map_err(err)? as usize;
                    if *m >= submaps {
                        return Err(bad("mapping mux"));
                    }
                }
            }
            let mut list = Vec::new();
            for _ in 0..submaps {
                b.read(8).map_err(err)?;
                let floor = b.read(8).map_err(err)? as usize;
                let residue = b.read(8).map_err(err)? as usize;
                if floor >= floors.len() || residue >= residues.len() {
                    return Err(bad("mapping submap"));
                }
                list.push((floor, residue));
            }
            mappings.push(Mapping { coupling, mux, submaps: list });
        }
        let count = b.read(6).map_err(err)? + 1;
        let mut modes = Vec::new();
        for _ in 0..count {
            let long = b.bit().map_err(err)?;
            let window = b.read(16).map_err(err)?;
            let transform = b.read(16).map_err(err)?;
            let mapping = b.read(8).map_err(err)? as usize;
            if window != 0 || transform != 0 || mapping >= mappings.len() {
                return Err(bad("mode"));
            }
            modes.push(Mode { long, mapping });
        }
        let [short, long] = id.block;
        Ok(Vorbis {
            channels: id.channels,
            sample_rate: id.sample_rate,
            block: id.block,
            books,
            floors,
            residues,
            mappings,
            modes,
            imdct: [Imdct::new(short), Imdct::new(long)],
            slopes: [slope(short / 2), slope(long / 2)],
            prev: Vec::new(),
        })
    }

    /// Decodes an audio packet and appends its samples to `out` (one vector
    /// per channel); returns how many. A packet that cannot be decoded
    /// gives nothing (the next one starts over without overlap).
    pub fn decode(&mut self, packet: &[u8], out: &mut [Vec<f32>]) -> usize {
        let mut b = Bits::new(packet);
        match self.decode_block(&mut b) {
            Some(block) => self.overlap(block, out),
            None => 0,
        }
    }

    /// Windowed block of each channel.
    fn decode_block(&self, b: &mut Bits) -> Option<Vec<Vec<f32>>> {
        if b.bit().ok()? {
            return None;
        }
        let mode = &self.modes.get(b.read(ilog(self.modes.len() as u32 - 1)).ok()? as usize)?;
        let n = self.block[mode.long as usize];
        let (prev_long, next_long) = if mode.long { (b.bit().ok()?, b.bit().ok()?) } else { (false, false) };
        let mapping = &self.mappings[mode.mapping];
        let half = n / 2;
        let ch = self.channels;
        // Floors; the end of the packet leaves a channel silent.
        let mut floors: Vec<Option<FloorData>> = Vec::with_capacity(ch);
        for c in 0..ch {
            let floor = &self.floors[mapping.submaps[mapping.mux[c]].0];
            floors.push(floor.decode(b, &self.books).ok().flatten());
        }
        let mut skip: Vec<bool> = floors.iter().map(Option::is_none).collect();
        for &(m, a) in &mapping.coupling {
            if !skip[m] || !skip[a] {
                skip[m] = false;
                skip[a] = false;
            }
        }
        // Residues, per submap.
        let mut spectra = vec![vec![0f32; half]; ch];
        for (s, &(_, residue)) in mapping.submaps.iter().enumerate() {
            let chans: Vec<usize> = (0..ch).filter(|&c| mapping.mux[c] == s).collect();
            let mut vectors: Vec<Vec<f32>> = chans.iter().map(|&c| std::mem::take(&mut spectra[c])).collect();
            let flags: Vec<bool> = chans.iter().map(|&c| skip[c]).collect();
            // A short packet keeps what was read.
            let _ = self.residues[residue].decode(b, &self.books, &mut vectors, &flags, half);
            for (&c, v) in chans.iter().zip(vectors) {
                spectra[c] = v;
            }
        }
        // Inverse coupling, last step first.
        for &(m, a) in mapping.coupling.iter().rev() {
            let (mag, ang) = if m < a {
                let (x, y) = spectra.split_at_mut(a);
                (&mut x[m], &mut y[0])
            } else {
                let (x, y) = spectra.split_at_mut(m);
                (&mut y[0], &mut x[a])
            };
            for (mv, av) in mag.iter_mut().zip(ang.iter_mut()) {
                let (m, a) = (*mv, *av);
                let (nm, na) = if m > 0.0 {
                    if a > 0.0 { (m, m - a) } else { (m + a, m) }
                } else if a > 0.0 {
                    (m, m + a)
                } else {
                    (m - a, m)
                };
                *mv = nm;
                *av = na;
            }
        }
        // Floor curves times the residues, then the IMDCT and the window.
        let mut curve = vec![0f32; half];
        let mut blocks = Vec::with_capacity(ch);
        for c in 0..ch {
            let mut y = vec![0f32; n];
            if let Some(data) = &floors[c] {
                self.floors[mapping.submaps[mapping.mux[c]].0].curve(data, half, &mut curve);
                for (s, f) in spectra[c].iter_mut().zip(&curve) {
                    *s *= f;
                }
                self.imdct[mode.long as usize].run(&spectra[c], &mut y);
                self.window(&mut y, mode.long, prev_long, next_long);
            }
            blocks.push(y);
        }
        Some(blocks)
    }

    fn window(&self, y: &mut [f32], long: bool, prev_long: bool, next_long: bool) {
        let n = y.len();
        let short = self.block[0];
        let (left_start, left) = if long && !prev_long { (n / 4 - short / 4, &self.slopes[0]) } else { (0, &self.slopes[long as usize]) };
        let (right_start, right) = if long && !next_long { (n * 3 / 4 - short / 4, &self.slopes[0]) } else { (n / 2, &self.slopes[long as usize]) };
        y[..left_start].fill(0.0);
        for (v, w) in y[left_start..].iter_mut().zip(left) {
            *v *= w;
        }
        for (v, w) in y[right_start..].iter_mut().zip(right.iter().rev()) {
            *v *= w;
        }
        y[right_start + right.len()..].fill(0.0);
    }

    /// Overlap-adds the new block with the previous one and returns the
    /// finished samples: from the middle of the previous block to the
    /// middle of this one.
    fn overlap(&mut self, block: Vec<Vec<f32>>, out: &mut [Vec<f32>]) -> usize {
        let n = block[0].len();
        if self.prev.is_empty() {
            self.prev = block;
            return 0;
        }
        let pn = self.prev[0].len();
        let len = pn / 4 + n / 4;
        for ((prev, cur), o) in self.prev.iter().zip(&block).zip(out.iter_mut()) {
            o.reserve(len);
            for j in 0..len {
                let a = prev.get(pn / 2 + j).copied().unwrap_or(0.0);
                let k = (j + n / 4).checked_sub(pn / 4);
                let b = k.and_then(|k| cur.get(k)).copied().unwrap_or(0.0);
                o.push(a + b);
            }
        }
        self.prev = block;
        len
    }

    /// Forgets the previous block (after a lost packet).
    pub fn reset(&mut self) {
        self.prev.clear();
    }
}

/// Floor 1 amplitudes: 0 to 255 in steps of about 0.55 dB.
#[allow(clippy::excessive_precision)]
const INVERSE_DB: [f32; 256] = [
    1.0649863e-07, 1.1341951e-07, 1.2079015e-07, 1.2863978e-07, 1.3699951e-07, 1.4590251e-07, 1.5538408e-07, 1.6548181e-07,
    1.7623575e-07, 1.8768855e-07, 1.9988561e-07, 2.1287530e-07, 2.2670913e-07, 2.4144197e-07, 2.5713223e-07, 2.7384213e-07,
    2.9163793e-07, 3.1059021e-07, 3.3077411e-07, 3.5226968e-07, 3.7516214e-07, 3.9954229e-07, 4.2550680e-07, 4.5315863e-07,
    4.8260743e-07, 5.1396998e-07, 5.4737065e-07, 5.8294187e-07, 6.2082472e-07, 6.6116941e-07, 7.0413592e-07, 7.4989464e-07,
    7.9862701e-07, 8.5052630e-07, 9.0579828e-07, 9.6466216e-07, 1.0273513e-06, 1.0941144e-06, 1.1652161e-06, 1.2409384e-06,
    1.3215816e-06, 1.4074654e-06, 1.4989305e-06, 1.5963394e-06, 1.7000785e-06, 1.8105592e-06, 1.9282195e-06, 2.0535261e-06,
    2.1869758e-06, 2.3290978e-06, 2.4804557e-06, 2.6416497e-06, 2.8133190e-06, 2.9961443e-06, 3.1908506e-06, 3.3982101e-06,
    3.6190449e-06, 3.8542308e-06, 4.1047004e-06, 4.3714470e-06, 4.6555282e-06, 4.9580707e-06, 5.2802740e-06, 5.6234160e-06,
    5.9888572e-06, 6.3780469e-06, 6.7925283e-06, 7.2339451e-06, 7.7040476e-06, 8.2047000e-06, 8.7378876e-06, 9.3057248e-06,
    9.9104632e-06, 1.0554501e-05, 1.1240392e-05, 1.1970856e-05, 1.2748789e-05, 1.3577278e-05, 1.4459606e-05, 1.5399272e-05,
    1.6400004e-05, 1.7465768e-05, 1.8600792e-05, 1.9809576e-05, 2.1096914e-05, 2.2467911e-05, 2.3928002e-05, 2.5482978e-05,
    2.7139006e-05, 2.8902651e-05, 3.0780908e-05, 3.2781225e-05, 3.4911534e-05, 3.7180282e-05, 3.9596466e-05, 4.2169667e-05,
    4.4910090e-05, 4.7828601e-05, 5.0936773e-05, 5.4246931e-05, 5.7772202e-05, 6.1526565e-05, 6.5524908e-05, 6.9783085e-05,
    7.4317983e-05, 7.9147585e-05, 8.4291040e-05, 8.9768747e-05, 9.5602426e-05, 0.00010181521, 0.00010843174, 0.00011547824,
    0.00012298267, 0.00013097477, 0.00013948625, 0.00014855085, 0.00015820453, 0.00016848555, 0.00017943469, 0.00019109536,
    0.00020351382, 0.00021673929, 0.00023082423, 0.00024582449, 0.00026179955, 0.00027881276, 0.00029693158, 0.00031622787,
    0.00033677814, 0.00035866388, 0.00038197188, 0.00040679456, 0.00043323036, 0.00046138411, 0.00049136745, 0.00052329927,
    0.00055730621, 0.00059352311, 0.00063209358, 0.00067317058, 0.00071691700, 0.00076350630, 0.00081312324, 0.00086596457,
    0.00092223983, 0.00098217216, 0.0010459992, 0.0011139742, 0.0011863665, 0.0012634633, 0.0013455702, 0.0014330129,
    0.0015261382, 0.0016253153, 0.0017309374, 0.0018434235, 0.0019632195, 0.0020908006, 0.0022266726, 0.0023713743,
    0.0025254795, 0.0026895994, 0.0028643847, 0.0030505286, 0.0032487691, 0.0034598925, 0.0036847358, 0.0039241906,
    0.0041792066, 0.0044507950, 0.0047400328, 0.0050480668, 0.0053761186, 0.0057254891, 0.0060975636, 0.0064938176,
    0.0069158225, 0.0073652516, 0.0078438871, 0.0083536271, 0.0088964928, 0.009474637, 0.010090352, 0.010746080,
    0.011444421, 0.012188144, 0.012980198, 0.013823725, 0.014722068, 0.015678791, 0.016697687, 0.017782797,
    0.018938423, 0.020169149, 0.021479854, 0.022875735, 0.024362330, 0.025945531, 0.027631618, 0.029427276,
    0.031339626, 0.033376252, 0.035545228, 0.037855157, 0.040315199, 0.042935108, 0.045725273, 0.048696758,
    0.051861348, 0.055231591, 0.058820850, 0.062643361, 0.066714279, 0.071049749, 0.075666962, 0.080584227,
    0.085821044, 0.091398179, 0.097337747, 0.10366330, 0.11039993, 0.11757434, 0.12521498, 0.13335215,
    0.14201813, 0.15124727, 0.16107617, 0.17154380, 0.18269168, 0.19456402, 0.20720788, 0.22067342,
    0.23501402, 0.25028656, 0.26655159, 0.28387361, 0.30232132, 0.32196786, 0.34289114, 0.36517414,
    0.38890521, 0.41417847, 0.44109412, 0.46975890, 0.50028648, 0.53279791, 0.56742212, 0.60429640,
    0.64356699, 0.68538959, 0.72993007, 0.77736504, 0.82788260, 0.88168307, 0.9389798, 1.0,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imdct_matches_its_definition() {
        for n in [64usize, 256, 2048] {
            let imdct = Imdct::new(n);
            let x: Vec<f32> = (0..n / 2).map(|k| ((k * 7919 % 101) as f32 / 50.0 - 1.0) / (1.0 + k as f32 / 8.0)).collect();
            let mut y = vec![0f32; n];
            imdct.run(&x, &mut y);
            for (i, &got) in y.iter().enumerate() {
                let want: f64 = x
                    .iter()
                    .enumerate()
                    .map(|(k, &v)| v as f64 * (2.0 * PI / n as f64 * (i as f64 + 0.5 + n as f64 / 4.0) * (k as f64 + 0.5)).cos())
                    .sum();
                assert!((got as f64 - want).abs() < 1e-4, "n {n}, i {i}: {got} vs {want}");
            }
        }
    }

    #[test]
    fn small_helpers() {
        assert_eq!((ilog(0), ilog(1), ilog(7), ilog(8)), (0, 1, 3, 4));
        assert_eq!(lookup1_values(81, 4), 3);
        assert_eq!(lookup1_values(80, 4), 2);
        assert_eq!(lookup1_values(1, 1), 1);
        // 1.0: mantissa 2^20, exponent 788 - 20.
        assert_eq!(float32((768 << 21) | (1 << 20)), 1.0);
        assert_eq!(float32(0x8000_0000 | (768 << 21) | (1 << 20)), -1.0);
    }

    #[test]
    fn codewords_follow_the_specification_example() {
        // The example of the specification: lengths 2 4 4 4 4 2 3 3.
        let (tree, single) = Codebook::build_tree(&[2, 4, 4, 4, 4, 2, 3, 3]).unwrap();
        assert!(single.is_none());
        let book = Codebook { dims: 1, tree, single, vq: None };
        // Codewords 00, 0100, 0101, 0110, 0111, 10, 110, 111 (written MSB
        // first, read one bit at a time).
        let codes = ["00", "0100", "0101", "0110", "0111", "10", "110", "111"];
        for (entry, code) in codes.iter().enumerate() {
            let mut bytes = [0u8; 1];
            for (k, c) in code.chars().enumerate() {
                if c == '1' {
                    bytes[0] |= 1 << k;
                }
            }
            assert_eq!(book.decode(&mut Bits::new(&bytes)).unwrap(), entry);
        }
        assert!(Codebook::build_tree(&[1, 1, 1]).is_err(), "overspecified");
    }
}
