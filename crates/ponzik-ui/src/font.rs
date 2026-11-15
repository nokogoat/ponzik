//! Lecteur TrueType maison : tables head, hhea, hmtx, maxp, cmap (formats 4 et 12),
//! loca, glyf (glyphes simples et composites) et crénage GPOS (PairPos 1 et 2).

/// Point de contour en unités de police, y vers le haut.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Point {
    pub x: f32,
    pub y: f32,
    pub on: bool,
}

pub(crate) type Contour = Vec<Point>;

pub(crate) struct Font {
    data: &'static [u8],
    pub units_per_em: f32,
    pub ascent: f32,
    pub descent: f32,
    pub cap_height: f32,
    num_glyphs: u16,
    loca: usize,
    loca_long: bool,
    glyf: usize,
    hmtx: usize,
    num_hmetrics: u16,
    cmap: usize,
    cmap_format: u16,
    /// Pour chaque lookup de crénage : ses sous-tables PairPos (offsets absolus).
    kern_lookups: Vec<Vec<usize>>,
}

impl Font {
    pub(crate) fn parse(data: &'static [u8]) -> Result<Font, String> {
        let mut f = Font {
            data,
            units_per_em: 0.0,
            ascent: 0.0,
            descent: 0.0,
            cap_height: 0.0,
            num_glyphs: 0,
            loca: 0,
            loca_long: false,
            glyf: 0,
            hmtx: 0,
            num_hmetrics: 0,
            cmap: 0,
            cmap_format: 0,
            kern_lookups: Vec::new(),
        };
        let be16 = |o: usize| data.get(o..o + 2).map_or(0, |b| u16::from_be_bytes([b[0], b[1]]) as usize);
        let be32 = |o: usize| data.get(o..o + 4).map_or(0, |b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize);
        let table = |tag: &[u8; 4]| -> Option<usize> {
            (0..be16(4)).map(|i| 12 + 16 * i).find(|&r| data.get(r..r + 4) == Some(tag)).map(|r| be32(r + 8))
        };
        let need = |tag: &[u8; 4]| table(tag).ok_or_else(|| format!("table {} absente", String::from_utf8_lossy(tag)));
        let head = need(b"head")?;
        let hhea = need(b"hhea")?;
        let maxp = need(b"maxp")?;
        let cmap = need(b"cmap")?;
        f.loca = need(b"loca")?;
        f.glyf = need(b"glyf")?;
        f.hmtx = need(b"hmtx")?;
        let gpos = table(b"GPOS");

        f.units_per_em = f.u16(head + 18) as f32;
        f.loca_long = f.i16(head + 50) != 0;
        f.ascent = f.i16(hhea + 4) as f32;
        f.descent = f.i16(hhea + 6) as f32;
        f.num_hmetrics = f.u16(hhea + 34);
        f.num_glyphs = f.u16(maxp + 4);
        if f.units_per_em <= 0.0 || f.num_hmetrics == 0 {
            return Err("en-têtes de police invalides".into());
        }

        // Sous-table cmap : Unicode complet (format 12) de préférence, sinon BMP (format 4).
        let mut best: Option<(u16, usize)> = None;
        for i in 0..f.u16(cmap + 2) as usize {
            let rec = cmap + 4 + 8 * i;
            let (platform, encoding) = (f.u16(rec), f.u16(rec + 2));
            let sub = cmap + f.u32(rec + 4) as usize;
            let format = f.u16(sub);
            let unicode = platform == 0 || (platform == 3 && (encoding == 1 || encoding == 10));
            if unicode && (format == 12 || (format == 4 && best.is_none_or(|(fmt, _)| fmt != 12))) {
                best = Some((format, sub));
            }
        }
        let (format, sub) = best.ok_or("aucune table cmap Unicode")?;
        f.cmap_format = format;
        f.cmap = sub;

        let h = f.glyph_index('H');
        f.cap_height = f.glyph_range(h).map(|(s, _)| f.i16(s + 8) as f32).unwrap_or(f.ascent * 0.7);

        if let Some(gpos) = gpos {
            f.kern_lookups = f.parse_kern_lookups(gpos);
        }
        Ok(f)
    }

    fn u16(&self, off: usize) -> u16 {
        self.data.get(off..off + 2).map_or(0, |b| u16::from_be_bytes([b[0], b[1]]))
    }

    fn i16(&self, off: usize) -> i16 {
        self.u16(off) as i16
    }

    fn u32(&self, off: usize) -> u32 {
        self.data.get(off..off + 4).map_or(0, |b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u8(&self, off: usize) -> u8 {
        self.data.get(off).copied().unwrap_or(0)
    }

    pub(crate) fn glyph_index(&self, c: char) -> u16 {
        let c = c as u32;
        let sub = self.cmap;
        match self.cmap_format {
            4 => {
                if c > 0xffff {
                    return 0;
                }
                let seg_x2 = self.u16(sub + 6) as usize;
                let ends = sub + 14;
                let starts = ends + seg_x2 + 2;
                let deltas = starts + seg_x2;
                let ranges = deltas + seg_x2;
                // Recherche dichotomique du premier segment dont la fin >= c.
                let (mut lo, mut hi) = (0, seg_x2 / 2);
                while lo < hi {
                    let mid = (lo + hi) / 2;
                    if (self.u16(ends + 2 * mid) as u32) < c {
                        lo = mid + 1;
                    } else {
                        hi = mid;
                    }
                }
                if lo >= seg_x2 / 2 {
                    return 0;
                }
                let start = self.u16(starts + 2 * lo) as u32;
                if c < start {
                    return 0;
                }
                let delta = self.u16(deltas + 2 * lo);
                let range = self.u16(ranges + 2 * lo) as usize;
                if range == 0 {
                    return (c as u16).wrapping_add(delta);
                }
                let at = ranges + 2 * lo + range + 2 * (c - start) as usize;
                match self.u16(at) {
                    0 => 0,
                    g => g.wrapping_add(delta),
                }
            }
            12 => {
                let groups = self.u32(sub + 12) as usize;
                let (mut lo, mut hi) = (0, groups);
                while lo < hi {
                    let mid = (lo + hi) / 2;
                    let g = sub + 16 + 12 * mid;
                    if self.u32(g + 4) < c {
                        lo = mid + 1;
                    } else {
                        hi = mid;
                    }
                }
                if lo >= groups {
                    return 0;
                }
                let g = sub + 16 + 12 * lo;
                let start = self.u32(g);
                if c < start { 0 } else { (self.u32(g + 8) + (c - start)) as u16 }
            }
            _ => 0,
        }
    }

    pub(crate) fn advance(&self, gid: u16) -> f32 {
        let i = gid.min(self.num_hmetrics - 1) as usize;
        self.u16(self.hmtx + 4 * i) as f32
    }

    fn glyph_range(&self, gid: u16) -> Option<(usize, usize)> {
        if gid >= self.num_glyphs {
            return None;
        }
        let i = gid as usize;
        let (a, b) = if self.loca_long {
            (self.u32(self.loca + 4 * i) as usize, self.u32(self.loca + 4 * i + 4) as usize)
        } else {
            (self.u16(self.loca + 2 * i) as usize * 2, self.u16(self.loca + 2 * i + 2) as usize * 2)
        };
        (b > a).then_some((self.glyf + a, self.glyf + b))
    }

    /// Contours du glyphe, en unités de police.
    pub(crate) fn outline(&self, gid: u16) -> Vec<Contour> {
        let mut out = Vec::new();
        self.outline_into(gid, &mut out, 0);
        out
    }

    fn outline_into(&self, gid: u16, out: &mut Vec<Contour>, depth: u32) {
        let Some((g, _)) = self.glyph_range(gid) else { return };
        let n = self.i16(g);
        if n >= 0 {
            self.simple_glyph(g, n as usize, out);
        } else if depth < 8 {
            self.composite_glyph(g, out, depth);
        }
    }

    fn simple_glyph(&self, g: usize, contours: usize, out: &mut Vec<Contour>) {
        let ends: Vec<usize> = (0..contours).map(|i| self.u16(g + 10 + 2 * i) as usize).collect();
        let Some(&last) = ends.last() else { return };
        let count = last + 1;
        let mut at = g + 10 + 2 * contours;
        at += 2 + self.u16(at) as usize; // instructions ignorées (pas de hinting)

        let mut flags = Vec::with_capacity(count);
        while flags.len() < count {
            let f = self.u8(at);
            at += 1;
            flags.push(f);
            if f & 0x08 != 0 {
                let repeat = self.u8(at);
                at += 1;
                for _ in 0..repeat {
                    flags.push(f);
                }
            }
        }
        flags.truncate(count);

        let mut read_coords = |short: u8, same: u8| -> Vec<f32> {
            let mut v = 0i32;
            flags
                .iter()
                .map(|&f| {
                    if f & short != 0 {
                        let d = self.u8(at) as i32;
                        at += 1;
                        v += if f & same != 0 { d } else { -d };
                    } else if f & same == 0 {
                        v += self.i16(at) as i32;
                        at += 2;
                    }
                    v as f32
                })
                .collect()
        };
        let xs = read_coords(0x02, 0x10);
        let ys = read_coords(0x04, 0x20);

        let mut start = 0;
        for &end in &ends {
            if end < start || end >= count {
                break;
            }
            out.push((start..=end).map(|i| Point { x: xs[i], y: ys[i], on: flags[i] & 1 != 0 }).collect());
            start = end + 1;
        }
    }

    fn composite_glyph(&self, g: usize, out: &mut Vec<Contour>, depth: u32) {
        let mut at = g + 10;
        loop {
            let flags = self.u16(at);
            let sub = self.u16(at + 2);
            at += 4;
            let (dx, dy) = if flags & 0x01 != 0 {
                let v = (self.i16(at) as f32, self.i16(at + 2) as f32);
                at += 4;
                v
            } else {
                let v = (self.u8(at) as i8 as f32, self.u8(at + 1) as i8 as f32);
                at += 2;
                v
            };
            let f2dot14 = |off: usize| self.i16(off) as f32 / 16384.0;
            let (a, b, c, d) = if flags & 0x08 != 0 {
                let s = f2dot14(at);
                at += 2;
                (s, 0.0, 0.0, s)
            } else if flags & 0x40 != 0 {
                let v = (f2dot14(at), 0.0, 0.0, f2dot14(at + 2));
                at += 4;
                v
            } else if flags & 0x80 != 0 {
                let v = (f2dot14(at), f2dot14(at + 2), f2dot14(at + 4), f2dot14(at + 6));
                at += 8;
                v
            } else {
                (1.0, 0.0, 0.0, 1.0)
            };
            // Sans ARGS_ARE_XY_VALUES, les arguments désignent des points à aligner : non géré.
            let (dx, dy) = if flags & 0x02 != 0 { (dx, dy) } else { (0.0, 0.0) };

            let mut parts = Vec::new();
            self.outline_into(sub, &mut parts, depth + 1);
            for contour in parts {
                out.push(
                    contour
                        .into_iter()
                        .map(|p| Point { x: a * p.x + c * p.y + dx, y: b * p.x + d * p.y + dy, on: p.on })
                        .collect(),
                );
            }
            if flags & 0x20 == 0 {
                break;
            }
        }
    }

    // --- Crénage GPOS ---

    fn parse_kern_lookups(&self, gpos: usize) -> Vec<Vec<usize>> {
        let features = gpos + self.u16(gpos + 6) as usize;
        let lookups = gpos + self.u16(gpos + 8) as usize;
        let mut indices: Vec<u16> = Vec::new();
        for i in 0..self.u16(features) as usize {
            let rec = features + 2 + 6 * i;
            if self.data.get(rec..rec + 4) != Some(b"kern") {
                continue;
            }
            let feature = features + self.u16(rec + 4) as usize;
            for j in 0..self.u16(feature + 2) as usize {
                let idx = self.u16(feature + 4 + 2 * j);
                if !indices.contains(&idx) {
                    indices.push(idx);
                }
            }
        }
        indices.sort_unstable();
        indices
            .into_iter()
            .map(|idx| {
                let lookup = lookups + self.u16(lookups + 2 + 2 * idx as usize) as usize;
                let kind = self.u16(lookup);
                (0..self.u16(lookup + 4) as usize)
                    .filter_map(|s| {
                        let sub = lookup + self.u16(lookup + 6 + 2 * s) as usize;
                        match kind {
                            2 => Some(sub),
                            // Extension : redirige vers la vraie sous-table.
                            9 if self.u16(sub + 2) == 2 => Some(sub + self.u32(sub + 4) as usize),
                            _ => None,
                        }
                    })
                    .collect()
            })
            .collect()
    }

    fn coverage(&self, cov: usize, gid: u16) -> Option<usize> {
        match self.u16(cov) {
            1 => {
                let (mut lo, mut hi) = (0, self.u16(cov + 2) as usize);
                while lo < hi {
                    let mid = (lo + hi) / 2;
                    match self.u16(cov + 4 + 2 * mid).cmp(&gid) {
                        std::cmp::Ordering::Less => lo = mid + 1,
                        std::cmp::Ordering::Greater => hi = mid,
                        std::cmp::Ordering::Equal => return Some(mid),
                    }
                }
                None
            }
            2 => {
                let (mut lo, mut hi) = (0, self.u16(cov + 2) as usize);
                while lo < hi {
                    let mid = (lo + hi) / 2;
                    let r = cov + 4 + 6 * mid;
                    if gid < self.u16(r) {
                        hi = mid;
                    } else if gid > self.u16(r + 2) {
                        lo = mid + 1;
                    } else {
                        return Some((self.u16(r + 4) + gid - self.u16(r)) as usize);
                    }
                }
                None
            }
            _ => None,
        }
    }

    fn class_of(&self, def: usize, gid: u16) -> usize {
        match self.u16(def) {
            1 => {
                let start = self.u16(def + 2);
                let n = self.u16(def + 4);
                if gid >= start && gid - start < n { self.u16(def + 6 + 2 * (gid - start) as usize) as usize } else { 0 }
            }
            2 => {
                let (mut lo, mut hi) = (0, self.u16(def + 2) as usize);
                while lo < hi {
                    let mid = (lo + hi) / 2;
                    let r = def + 4 + 6 * mid;
                    if gid < self.u16(r) {
                        hi = mid;
                    } else if gid > self.u16(r + 2) {
                        lo = mid + 1;
                    } else {
                        return self.u16(r + 4) as usize;
                    }
                }
                0
            }
            _ => 0,
        }
    }

    fn x_advance(&self, format: u16, rec: usize) -> i16 {
        if format & 0x4 == 0 { 0 } else { self.i16(rec + 2 * (format & 0x3).count_ones() as usize) }
    }

    fn pair_pos(&self, sub: usize, left: u16, right: u16) -> Option<i16> {
        let cov = sub + self.u16(sub + 2) as usize;
        let (vf1, vf2) = (self.u16(sub + 4), self.u16(sub + 6));
        let size1 = 2 * (vf1 & 0xff).count_ones() as usize;
        let size2 = 2 * (vf2 & 0xff).count_ones() as usize;
        let index = self.coverage(cov, left)?;
        match self.u16(sub) {
            1 => {
                if index >= self.u16(sub + 8) as usize {
                    return None;
                }
                let set = sub + self.u16(sub + 10 + 2 * index) as usize;
                let rec_size = 2 + size1 + size2;
                let (mut lo, mut hi) = (0, self.u16(set) as usize);
                while lo < hi {
                    let mid = (lo + hi) / 2;
                    let rec = set + 2 + rec_size * mid;
                    match self.u16(rec).cmp(&right) {
                        std::cmp::Ordering::Less => lo = mid + 1,
                        std::cmp::Ordering::Greater => hi = mid,
                        std::cmp::Ordering::Equal => return Some(self.x_advance(vf1, rec + 2)),
                    }
                }
                None
            }
            2 => {
                let c1 = self.class_of(sub + self.u16(sub + 8) as usize, left);
                let c2 = self.class_of(sub + self.u16(sub + 10) as usize, right);
                let (n1, n2) = (self.u16(sub + 12) as usize, self.u16(sub + 14) as usize);
                if c1 >= n1 || c2 >= n2 {
                    return Some(0);
                }
                Some(self.x_advance(vf1, sub + 16 + (c1 * n2 + c2) * (size1 + size2)))
            }
            _ => None,
        }
    }

    /// Ajustement de chasse entre deux glyphes, en unités de police.
    pub(crate) fn kerning(&self, left: u16, right: u16) -> f32 {
        self.kern_lookups
            .iter()
            .map(|subs| subs.iter().find_map(|&s| self.pair_pos(s, left, right)).unwrap_or(0) as f32)
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embedded;

    #[test]
    fn parses_embedded_fonts() {
        for data in [embedded::SANS_REGULAR, embedded::SANS_MEDIUM, embedded::MONO_REGULAR, embedded::MONO_MEDIUM] {
            let f = Font::parse(data).unwrap();
            assert_eq!(f.units_per_em, 1000.0);
            assert!(f.ascent > 0.0 && f.descent < 0.0);
            for c in "Ponzik é è à ç ô 0123456789 − ∞ · # %".chars().filter(|c| *c != ' ') {
                assert_ne!(f.glyph_index(c), 0, "glyphe manquant : {c}");
            }
        }
    }

    #[test]
    fn outlines_and_composites() {
        let f = Font::parse(embedded::SANS_REGULAR).unwrap();
        assert_eq!(f.outline(f.glyph_index('o')).len(), 2);
        // « é » est un composite : base + accent, donc au moins deux contours.
        assert!(f.outline(f.glyph_index('é')).len() >= 2);
        assert!(f.outline(f.glyph_index(' ')).is_empty());
    }

    #[test]
    fn mono_is_monospaced_and_sans_has_kerning() {
        let mono = Font::parse(embedded::MONO_REGULAR).unwrap();
        let w = mono.advance(mono.glyph_index('0'));
        assert!("123456789.".chars().all(|c| mono.advance(mono.glyph_index(c)) == w));

        let sans = Font::parse(embedded::SANS_REGULAR).unwrap();
        let (a, v) = (sans.glyph_index('A'), sans.glyph_index('V'));
        assert!(sans.kerning(a, v) < 0.0, "AV devrait se resserrer");
    }
}
