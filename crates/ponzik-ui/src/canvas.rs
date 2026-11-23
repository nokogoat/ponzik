use std::f32::consts::TAU;

use crate::color::{Color, blend};
use crate::geom::Rect;
use crate::raster::Rasterizer;

type Pt = (f32, f32);

#[derive(Clone, Copy, Debug)]
enum Seg {
    Line(Pt, Pt),
    Quad(Pt, Pt, Pt),
}

/// Chemin vectoriel fermé : lignes et courbes quadratiques.
#[derive(Clone, Debug, Default)]
pub struct Path {
    segs: Vec<Seg>,
    start: Pt,
    cur: Pt,
}

impl Path {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn move_to(&mut self, p: Pt) {
        self.close();
        self.start = p;
        self.cur = p;
    }

    pub fn line_to(&mut self, p: Pt) {
        self.segs.push(Seg::Line(self.cur, p));
        self.cur = p;
    }

    pub fn quad_to(&mut self, c: Pt, p: Pt) {
        self.segs.push(Seg::Quad(self.cur, c, p));
        self.cur = p;
    }

    /// Vide le chemin en gardant sa mémoire.
    pub fn clear(&mut self) {
        self.segs.clear();
        self.start = (0.0, 0.0);
        self.cur = (0.0, 0.0);
    }

    pub fn close(&mut self) {
        if self.cur != self.start {
            self.segs.push(Seg::Line(self.cur, self.start));
            self.cur = self.start;
        }
    }

    /// Ajoute un polygone fermé, toujours dans le même sens de rotation pour que
    /// les recouvrements s'additionnent au lieu de s'annuler.
    pub fn polygon(&mut self, pts: &[Pt]) {
        let n = pts.len();
        if n < 3 {
            return;
        }
        self.close();
        let area: f32 = (0..n)
            .map(|i| {
                let (a, b) = (pts[i], pts[(i + 1) % n]);
                a.0 * b.1 - b.0 * a.1
            })
            .sum();
        let at = |i: usize| if area >= 0.0 { pts[i] } else { pts[n - 1 - i] };
        for i in 0..n {
            self.segs.push(Seg::Line(at(i), at((i + 1) % n)));
        }
    }

    fn bounds(&self) -> Option<(f32, f32, f32, f32)> {
        let mut b: Option<(f32, f32, f32, f32)> = None;
        let mut add = |p: Pt| {
            b = Some(match b {
                None => (p.0, p.1, p.0, p.1),
                Some((x0, y0, x1, y1)) => (x0.min(p.0), y0.min(p.1), x1.max(p.0), y1.max(p.1)),
            });
        };
        for s in &self.segs {
            match *s {
                Seg::Line(a, c) => {
                    add(a);
                    add(c);
                }
                Seg::Quad(a, c, d) => {
                    add(a);
                    add(c);
                    add(d);
                }
            }
        }
        b
    }

    /// Rastérise le chemin mis à l'échelle `k` puis décalé de (`dx`, `dy`).
    pub(crate) fn rasterize(&self, r: &mut Rasterizer, k: f32, dx: f32, dy: f32) {
        rasterize_segs(&self.segs, r, k, dx, dy);
        // Un contour resté ouvert est refermé.
        if self.cur != self.start {
            rasterize_segs(&[Seg::Line(self.cur, self.start)], r, k, dx, dy);
        }
    }
}

fn rasterize_segs(segs: &[Seg], r: &mut Rasterizer, k: f32, dx: f32, dy: f32) {
    let t = |p: Pt| (p.0 * k + dx, p.1 * k + dy);
    for s in segs {
        match *s {
            Seg::Line(a, b) => r.line(t(a), t(b)),
            Seg::Quad(a, c, b) => r.quad(t(a), t(c), t(b)),
        }
    }
}

/// Boîte en pixels physiques entiers : x0, y0, x1, y1 (bornes hautes exclues).
type PxBox = (usize, usize, usize, usize);

fn intersect_box(a: PxBox, b: PxBox) -> Option<PxBox> {
    let r = (a.0.max(b.0), a.1.max(b.1), a.2.min(b.2), a.3.min(b.3));
    (r.0 < r.2 && r.1 < r.3).then_some(r)
}

/// Côté des tuiles du suivi des zones modifiées, en pixels physiques.
const TILE: usize = 32;

/// Primitive enregistrée pendant une image suivie, en coordonnées physiques.
#[derive(Clone, Copy)]
enum Op {
    Clear(Color),
    Fill { r: Rect, radius: f32, top: Color, bottom: Color },
    Stroke { r: Rect, radius: f32, width: f32, c: Color },
    /// Segments `segs` de l'arène, en logique, à mettre à l'échelle `k`.
    Path { segs: (u32, u32), k: f32, c: Color },
    /// Masque 8 bits de l'arène, à partir de `at`.
    Mask { x: i32, y: i32, w: u32, h: u32, at: u32, c: Color },
}

struct Cmd {
    op: Op,
    /// Pixels que la primitive peut toucher (déjà découpés).
    bbox: PxBox,
}

/// Mélangeur 64 bits rapide (style FxHash), sensible à l'ordre.
#[inline]
fn mix(h: u64, v: u64) -> u64 {
    (h.rotate_left(5) ^ v).wrapping_mul(0x517c_c1b7_2722_0a95)
}

fn hash_color(h: u64, c: Color) -> u64 {
    mix(h, u32::from_le_bytes([c.r, c.g, c.b, c.a]) as u64)
}

fn hash_rect(h: u64, r: &Rect) -> u64 {
    let a = (r.x.to_bits() as u64) << 32 | r.y.to_bits() as u64;
    let b = (r.w.to_bits() as u64) << 32 | r.h.to_bits() as u64;
    mix(mix(h, a), b)
}

fn hash_segs(mut h: u64, segs: &[Seg]) -> u64 {
    let p = |h: u64, p: Pt| mix(h, (p.0.to_bits() as u64) << 32 | p.1.to_bits() as u64);
    for s in segs {
        h = match *s {
            Seg::Line(a, b) => p(p(mix(h, 1), a), b),
            Seg::Quad(a, c, b) => p(p(p(mix(h, 2), a), c), b),
        };
    }
    h
}

fn hash_bytes(mut h: u64, bytes: &[u8]) -> u64 {
    let mut chunks = bytes.chunks_exact(8);
    for c in &mut chunks {
        h = mix(h, u64::from_le_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]]));
    }
    for &b in chunks.remainder() {
        h = mix(h, b as u64);
    }
    h
}

/// Suivi des zones modifiées : l'image est enregistrée comme une liste de
/// primitives, chacune laisse son empreinte dans les tuiles qu'elle touche, et
/// seules les tuiles dont l'empreinte a changé depuis l'image précédente sont
/// redessinées — avec exactement les mêmes primitives, donc les mêmes pixels.
#[derive(Default)]
struct Damage {
    cmds: Vec<Cmd>,
    segs: Vec<Seg>,
    masks: Vec<u8>,
    cols: usize,
    rows: usize,
    hashes: Vec<u64>,
    prev: Vec<u64>,
    /// L'image précédente est à l'écran et `prev` la décrit.
    valid: bool,
    /// Zones redessinées par la dernière image, en pixels physiques.
    dirty: Vec<PxBox>,
    /// Bandes ouvertes pendant le regroupement des tuiles (gardées d'une image à l'autre).
    open: Vec<(usize, usize, usize)>,
    next: Vec<(usize, usize, usize)>,
}

impl Damage {
    fn reset(&mut self, width: usize, height: usize) {
        let (cols, rows) = (width.div_ceil(TILE), height.div_ceil(TILE));
        if (cols, rows) != (self.cols, self.rows) {
            (self.cols, self.rows) = (cols, rows);
            self.prev.clear();
            self.prev.resize(cols * rows, 0);
            self.valid = false;
        }
        self.cmds.clear();
        self.segs.clear();
        self.masks.clear();
        self.hashes.clear();
        self.hashes.resize(cols * rows, 0xcbf2_9ce4_8422_2325);
    }

    fn stamp(&mut self, bbox: PxBox, h: u64) {
        let (tx0, ty0) = (bbox.0 / TILE, bbox.1 / TILE);
        let (tx1, ty1) = ((bbox.2 - 1) / TILE, (bbox.3 - 1) / TILE);
        for ty in ty0..=ty1.min(self.rows.saturating_sub(1)) {
            let row = &mut self.hashes[ty * self.cols..(ty + 1) * self.cols];
            for t in &mut row[tx0..=tx1.min(self.cols - 1)] {
                *t = mix(*t, h);
            }
        }
    }

    /// Tuiles changées regroupées en rectangles disjoints (bandes horizontales
    /// fusionnées verticalement quand elles ont la même largeur).
    fn collect_dirty(&mut self, width: usize, height: usize) {
        self.dirty.clear();
        let full = !self.valid;
        // Rectangles ouverts : (colonne début, colonne fin, index dans `dirty`).
        let (mut open, mut next) = (std::mem::take(&mut self.open), std::mem::take(&mut self.next));
        open.clear();
        for ty in 0..self.rows {
            next.clear();
            let mut tx = 0;
            while tx < self.cols {
                let i = ty * self.cols + tx;
                if !(full || self.hashes[i] != self.prev[i]) {
                    tx += 1;
                    continue;
                }
                let start = tx;
                while tx < self.cols && (full || self.hashes[ty * self.cols + tx] != self.prev[ty * self.cols + tx]) {
                    tx += 1;
                }
                let (y0, y1) = (ty * TILE, ((ty + 1) * TILE).min(height));
                if let Some(&(_, _, idx)) = open.iter().find(|o| o.0 == start && o.1 == tx) {
                    self.dirty[idx].3 = y1;
                    next.push((start, tx, idx));
                } else {
                    self.dirty.push((start * TILE, y0, (tx * TILE).min(width), y1));
                    next.push((start, tx, self.dirty.len() - 1));
                }
            }
            std::mem::swap(&mut open, &mut next);
        }
        (self.open, self.next) = (open, next);
    }
}



/// Longueur de [c - 0.5, c + 0.5] ∩ [lo, hi].
#[inline]
fn overlap(c: f32, lo: f32, hi: f32) -> f32 {
    ((c + 0.5).min(hi) - (c - 0.5).max(lo)).clamp(0.0, 1.0)
}

/// Couverture d'un pixel (centre `cx`, `cy`) par un rectangle arrondi.
#[inline]
fn rrect_cov(r: &Rect, radius: f32, cx: f32, cy: f32) -> f32 {
    if radius > 0.5 {
        let qx = if cx < r.x + radius {
            r.x + radius - cx
        } else if cx > r.right() - radius {
            cx - (r.right() - radius)
        } else {
            -1.0
        };
        let qy = if cy < r.y + radius {
            r.y + radius - cy
        } else if cy > r.bottom() - radius {
            cy - (r.bottom() - radius)
        } else {
            -1.0
        };
        if qx > 0.0 && qy > 0.0 {
            let d = (qx * qx + qy * qy).sqrt() - radius;
            return (0.5 - d).clamp(0.0, 1.0);
        }
    }
    overlap(cx, r.x, r.right()) * overlap(cy, r.y, r.bottom())
}

/// Tampon de pixels `0x00RRGGBB` et primitives de dessin anti-aliasées.
///
/// Les primitives prennent des coordonnées logiques ; l'échelle ([`Canvas::set_scale`])
/// les convertit en pixels physiques avant la rastérisation, si bien qu'un écran
/// HiDPI reçoit des bords et du texte nets plutôt qu'une image agrandie.
///
/// Entre [`Canvas::begin_frame`] et [`Canvas::end_frame`], les primitives sont
/// enregistrées au lieu d'être dessinées : seules les zones qui diffèrent de
/// l'image précédente sont ensuite rastérisées (voir [`Canvas::dirty_rects`]).
pub struct Canvas {
    /// Taille du tampon, en pixels physiques.
    width: usize,
    height: usize,
    pixels: Vec<u32>,
    /// Pixels physiques par pixel logique.
    scale: f32,
    /// Zone de découpe en pixels physiques entiers : x0, y0, x1, y1.
    clip: PxBox,
    /// La même zone telle que demandée, en logique (`None` : tout le canvas). La
    /// rendre telle quelle garde `push_clip` / `set_clip` exactement réversibles.
    clip_logical: Option<Rect>,
    raster: Rasterizer,
    /// Image en cours d'enregistrement (entre `begin_frame` et `end_frame`).
    recording: bool,
    damage: Damage,
    /// Chemin et points de travail des traits et des arcs, gardés d'un appel
    /// à l'autre : dessiner un potard n'alloue rien.
    scratch_path: Path,
    scratch_pts: Vec<Pt>,
    /// Style du thème : rayons des coins (petit, moyen, grand) qui remplacent
    /// 4, 8 et 16 px, et épaisseur des contours (1 = inchangée).
    radii: [f32; 3],
    border: f32,
}

/// Rayons de référence : un thème qui les garde dessine exactement comme avant.
pub const DEFAULT_RADII: [f32; 3] = [4.0, 8.0, 16.0];

impl Canvas {
    pub fn new(width: usize, height: usize) -> Self {
        let mut c = Self {
            width: 0,
            height: 0,
            pixels: Vec::new(),
            scale: 1.0,
            clip: (0, 0, 0, 0),
            clip_logical: None,
            raster: Rasterizer::new(),
            recording: false,
            damage: Damage::default(),
            scratch_path: Path::new(),
            scratch_pts: Vec::new(),
            radii: DEFAULT_RADII,
            border: 1.0,
        };
        c.resize(width, height);
        c
    }

    /// Change la taille du tampon, en pixels physiques.
    pub fn resize(&mut self, width: usize, height: usize) {
        self.width = width;
        self.height = height;
        self.pixels.clear();
        self.pixels.shrink_to(width * height);
        self.pixels.resize(width * height, 0);
        self.set_clip(None);
        self.invalidate();
    }

    /// Largeur du tampon, en pixels physiques.
    pub fn width(&self) -> usize {
        self.width
    }

    /// Hauteur du tampon, en pixels physiques.
    pub fn height(&self) -> usize {
        self.height
    }

    pub fn pixels(&self) -> &[u32] {
        &self.pixels
    }

    /// Pixels physiques par pixel logique (1 par défaut).
    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// Change l'échelle logique → physique ; la découpe revient à tout le canvas.
    pub fn set_scale(&mut self, scale: f32) {
        let scale = if scale.is_finite() { scale.clamp(0.25, 8.0) } else { 1.0 };
        if scale != self.scale {
            self.scale = scale;
            self.set_clip(None);
            self.invalidate();
        }
    }

    /// Style des formes : `radii` (petit, moyen, grand) remplace les rayons de
    /// 4, 8 et 16 px (interpolés entre eux), `border` multiplie l'épaisseur des
    /// contours. Les pastilles et les cercles (rayon = demi-côté) restent ronds.
    pub fn set_style(&mut self, radii: [f32; 3], border: f32) {
        let ok = |v: f32| if v.is_finite() { v.clamp(0.0, 64.0) } else { 0.0 };
        self.radii = radii.map(ok);
        self.border = if border.is_finite() { border.clamp(0.0, 8.0) } else { 1.0 };
    }

    /// Rayon logique après le style du thème.
    fn styled_radius(&self, r: Rect, radius: f32) -> f32 {
        if self.radii == DEFAULT_RADII || radius <= 0.0 || radius >= r.w.min(r.h) * 0.5 - 1e-3 {
            return radius;
        }
        let [s, m, l] = self.radii;
        if radius <= 4.0 {
            radius / 4.0 * s
        } else if radius <= 8.0 {
            s + (radius - 4.0) / 4.0 * (m - s)
        } else if radius <= 16.0 {
            m + (radius - 8.0) / 8.0 * (l - m)
        } else {
            l * radius / 16.0
        }
    }

    /// Taille du canvas en pixels logiques : l'espace de mise en page.
    pub fn logical_size(&self) -> (f32, f32) {
        (self.width as f32 / self.scale, self.height as f32 / self.scale)
    }

    /// Rectangle logique → physique.
    #[inline]
    pub fn to_physical(&self, r: Rect) -> Rect {
        let k = self.scale;
        if k == 1.0 { r } else { Rect::new(r.x * k, r.y * k, r.w * k, r.h * k) }
    }

    /// Restreint le dessin à `r` (ou à tout le canvas avec `None`), en logique.
    pub fn set_clip(&mut self, r: Option<Rect>) {
        self.clip_logical = r;
        self.clip = match r {
            None => (0, 0, self.width, self.height),
            Some(r) => {
                let r = self.to_physical(r);
                let x0 = (r.x.floor().max(0.0) as usize).min(self.width);
                let y0 = (r.y.floor().max(0.0) as usize).min(self.height);
                let x1 = (r.right().ceil().max(0.0) as usize).clamp(x0, self.width);
                let y1 = (r.bottom().ceil().max(0.0) as usize).clamp(y0, self.height);
                (x0, y0, x1, y1)
            }
        };
    }

    /// Zone de découpe courante, en logique.
    pub fn clip_rect(&self) -> Rect {
        self.clip_logical.unwrap_or_else(|| {
            let (w, h) = self.logical_size();
            Rect::new(0.0, 0.0, w, h)
        })
    }

    /// Zone de découpe courante, en pixels physiques entiers.
    pub fn clip_pixels(&self) -> (usize, usize, usize, usize) {
        self.clip
    }

    /// Restreint la découpe à son intersection avec `r` ; renvoie l'ancienne.
    pub fn push_clip(&mut self, r: Rect) -> Rect {
        let prev = self.clip_rect();
        self.set_clip(Some(prev.intersect(&r)));
        prev
    }

    /// Commence une image suivie : les primitives sont enregistrées, puis
    /// [`Canvas::end_frame`] ne redessine que ce qui a changé.
    pub fn begin_frame(&mut self) {
        self.recording = true;
        self.damage.reset(self.width, self.height);
    }

    /// Termine l'image suivie : rastérise les zones modifiées et renvoie leur
    /// liste (aussi disponible par [`Canvas::dirty_rects`]). Vide si rien n'a changé.
    pub fn end_frame(&mut self) -> &[PxBox] {
        if !self.recording {
            return &[];
        }
        self.recording = false;
        let Canvas { damage, pixels, raster, width, height, .. } = self;
        damage.collect_dirty(*width, *height);
        for &d in &damage.dirty {
            for cmd in &damage.cmds {
                if let Some(b) = intersect_box(cmd.bbox, d) {
                    draw_op(pixels, *width, raster, &damage.segs, &damage.masks, &cmd.op, cmd.bbox, b);
                }
            }
        }
        std::mem::swap(&mut damage.hashes, &mut damage.prev);
        damage.valid = true;
        &damage.dirty
    }

    /// Zones (x0, y0, x1, y1) redessinées par la dernière image suivie, en pixels physiques.
    pub fn dirty_rects(&self) -> &[PxBox] {
        &self.damage.dirty
    }

    /// Oublie l'image précédente : la prochaine image suivie redessine tout.
    pub fn invalidate(&mut self) {
        self.damage.valid = false;
    }

    /// Nombre de primitives enregistrées par la dernière image suivie.
    pub fn recorded_ops(&self) -> usize {
        self.damage.cmds.len()
    }

    /// Dessine tout de suite, ou enregistre pendant une image suivie.
    fn emit(&mut self, op: Op, bbox: PxBox) {
        if !self.recording {
            let Canvas { pixels, raster, width, .. } = self;
            draw_op(pixels, *width, raster, &[], &[], &op, bbox, bbox);
            return;
        }
        let h = match op {
            Op::Clear(c) => hash_color(mix(0, 1), c),
            Op::Fill { r, radius, top, bottom } => {
                hash_color(hash_color(mix(hash_rect(mix(0, 2), &r), radius.to_bits() as u64), top), bottom)
            }
            Op::Stroke { r, radius, width, c } => hash_color(
                mix(mix(hash_rect(mix(0, 3), &r), radius.to_bits() as u64), width.to_bits() as u64),
                c,
            ),
            Op::Path { segs, k, c } => {
                let segs = &self.damage.segs[segs.0 as usize..segs.1 as usize];
                hash_color(hash_segs(mix(mix(0, 4), k.to_bits() as u64), segs), c)
            }
            Op::Mask { x, y, w, h, at, c } => {
                let bytes = &self.damage.masks[at as usize..at as usize + (w * h) as usize];
                let pos = (x as u32 as u64) << 32 | y as u32 as u64;
                hash_color(hash_bytes(mix(mix(mix(0, 5), pos), (w as u64) << 32 | h as u64), bytes), c)
            }
        };
        // La boîte entre dans l'empreinte : une découpe différente change les pixels.
        let h = mix(mix(h, (bbox.0 as u64) << 32 | bbox.1 as u64), (bbox.2 as u64) << 32 | bbox.3 as u64);
        self.damage.stamp(bbox, h);
        self.damage.cmds.push(Cmd { op, bbox });
    }

    pub fn clear(&mut self, c: Color) {
        if self.width == 0 || self.height == 0 {
            return;
        }
        self.emit(Op::Clear(c), (0, 0, self.width, self.height));
    }

    fn bounds(&self, x0: f32, y0: f32, x1: f32, y1: f32) -> Option<PxBox> {
        let (cx0, cy0, cx1, cy1) = self.clip;
        let bx0 = (x0.floor().max(0.0) as usize).max(cx0);
        let by0 = (y0.floor().max(0.0) as usize).max(cy0);
        let bx1 = (x1.ceil().max(0.0) as usize).min(cx1);
        let by1 = (y1.ceil().max(0.0) as usize).min(cy1);
        (bx0 < bx1 && by0 < by1).then_some((bx0, by0, bx1, by1))
    }

    pub fn fill_rect(&mut self, r: Rect, c: Color) {
        self.fill_rrect(r, 0.0, c);
    }

    pub fn fill_rrect(&mut self, r: Rect, radius: f32, c: Color) {
        self.fill_rrect_with(r, radius, c, c);
    }

    /// Dégradé vertical, réservé aux nuances d'une même teinte.
    pub fn fill_rrect_vgrad(&mut self, r: Rect, radius: f32, top: Color, bottom: Color) {
        self.fill_rrect_with(r, radius, top, bottom);
    }

    fn fill_rrect_with(&mut self, r: Rect, radius: f32, top: Color, bottom: Color) {
        if r.w <= 0.0 || r.h <= 0.0 || (top.a == 0 && bottom.a == 0) {
            return;
        }
        let radius = self.styled_radius(r, radius);
        let (r, radius) = (self.to_physical(r), radius * self.scale);
        let radius = radius.clamp(0.0, r.w.min(r.h) * 0.5);
        let Some(bbox) = self.bounds(r.x, r.y, r.right(), r.bottom()) else { return };
        self.emit(Op::Fill { r, radius, top, bottom }, bbox);
    }

    /// Contour intérieur d'épaisseur `width`.
    pub fn stroke_rrect(&mut self, r: Rect, radius: f32, width: f32, c: Color) {
        if r.w <= 0.0 || r.h <= 0.0 || c.a == 0 {
            return;
        }
        let (radius, width) = (self.styled_radius(r, radius), width * self.border);
        if width <= 0.0 {
            return;
        }
        let (r, radius, width) = (self.to_physical(r), radius * self.scale, width * self.scale);
        let radius = radius.clamp(0.0, r.w.min(r.h) * 0.5);
        let Some(bbox) = self.bounds(r.x, r.y, r.right(), r.bottom()) else { return };
        self.emit(Op::Stroke { r, radius, width, c }, bbox);
    }

    pub fn fill_circle(&mut self, cx: f32, cy: f32, radius: f32, c: Color) {
        self.fill_rrect(Rect::new(cx - radius, cy - radius, radius * 2.0, radius * 2.0), radius, c);
    }

    pub fn stroke_circle(&mut self, cx: f32, cy: f32, radius: f32, width: f32, c: Color) {
        self.stroke_rrect(Rect::new(cx - radius, cy - radius, radius * 2.0, radius * 2.0), radius, width, c);
    }

    pub fn fill_path(&mut self, path: &Path, c: Color) {
        if c.a == 0 {
            return;
        }
        let Some((bx0, by0, bx1, by1)) = path.bounds() else { return };
        let k = self.scale;
        let Some(bbox) = self.bounds(bx0 * k, by0 * k, bx1 * k, by1 * k) else { return };
        if !self.recording {
            let Canvas { raster, pixels, width, .. } = self;
            let (x0, y0, x1, y1) = bbox;
            raster.reset(x1 - x0, y1 - y0);
            path.rasterize(raster, k, -(x0 as f32), -(y0 as f32));
            raster.for_each_coverage(|x, y, cov| blend(&mut pixels[(y0 + y) * *width + x0 + x], c, cov));
            return;
        }
        let start = self.damage.segs.len();
        self.damage.segs.extend_from_slice(&path.segs);
        if path.cur != path.start {
            self.damage.segs.push(Seg::Line(path.cur, path.start));
        }
        let segs = (start as u32, self.damage.segs.len() as u32);
        self.emit(Op::Path { segs, k, c }, bbox);
    }

    /// Ligne brisée d'épaisseur `width`, jointures arrondies.
    pub fn stroke_polyline(&mut self, pts: &[Pt], width: f32, c: Color) {
        let mut path = std::mem::take(&mut self.scratch_path);
        path.clear();
        let hw = width * 0.5;
        for w in pts.windows(2) {
            let (a, b) = (w[0], w[1]);
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            let len = (dx * dx + dy * dy).sqrt();
            if len < 1e-4 {
                continue;
            }
            let (nx, ny) = (-dy / len * hw, dx / len * hw);
            path.polygon(&[(a.0 + nx, a.1 + ny), (b.0 + nx, b.1 + ny), (b.0 - nx, b.1 - ny), (a.0 - nx, a.1 - ny)]);
        }
        // Round joins only where the line turns: on a straight or gently bent run,
        // the segments already meet and a disc would add coverage on the edge
        // pixels, beading the line.
        if width > 1.5 {
            for w in pts.windows(3) {
                let (a, p, b) = (w[0], w[1], w[2]);
                let (d1, d2) = ((p.0 - a.0, p.1 - a.1), (b.0 - p.0, b.1 - p.1));
                let (l1, l2) = ((d1.0 * d1.0 + d1.1 * d1.1).sqrt(), (d2.0 * d2.0 + d2.1 * d2.1).sqrt());
                // Turn angle above ~0.1 rad (the gap it leaves stays under a tenth of a pixel below).
                if l1 * l2 < 1e-8 || (d1.0 * d2.0 + d1.1 * d2.1) < 0.995 * l1 * l2 {
                    path.polygon(&disc::<10>(p, hw));
                }
            }
        }
        self.fill_path(&path, c);
        self.scratch_path = path;
    }

    /// Arc de cercle épais, angles en radians (0 = droite, sens horaire à l'écran), bouts arrondis.
    pub fn stroke_arc(&mut self, cx: f32, cy: f32, radius: f32, a0: f32, a1: f32, width: f32, c: Color) {
        let (a0, a1) = if a0 <= a1 { (a0, a1) } else { (a1, a0) };
        let sweep = a1 - a0;
        if sweep <= 1e-4 {
            return;
        }
        let n = ((sweep * radius * self.scale / 2.0).ceil() as usize).clamp(4, 256);
        let (ro, ri) = (radius + width * 0.5, radius - width * 0.5);
        let mut pts = std::mem::take(&mut self.scratch_pts);
        pts.clear();
        for i in 0..=n {
            let a = a0 + sweep * i as f32 / n as f32;
            pts.push((cx + a.cos() * ro, cy + a.sin() * ro));
        }
        for i in (0..=n).rev() {
            let a = a0 + sweep * i as f32 / n as f32;
            pts.push((cx + a.cos() * ri, cy + a.sin() * ri));
        }
        let mut path = std::mem::take(&mut self.scratch_path);
        path.clear();
        path.polygon(&pts);
        for a in [a0, a1] {
            path.polygon(&disc::<12>((cx + a.cos() * radius, cy + a.sin() * radius), width * 0.5));
        }
        self.fill_path(&path, c);
        (self.scratch_path, self.scratch_pts) = (path, pts);
    }

    /// Masque d'opacité 8 bits (glyphes), coin haut-gauche en (`x`, `y`), en
    /// pixels physiques : le masque est déjà rastérisé à la bonne échelle.
    pub fn draw_mask(&mut self, x: i32, y: i32, w: usize, h: usize, mask: &[u8], c: Color) {
        if w == 0 || h == 0 || c.a == 0 {
            return;
        }
        let (cx0, cy0, cx1, cy1) = self.clip;
        let bx0 = (x.max(0) as usize).max(cx0);
        let by0 = (y.max(0) as usize).max(cy0);
        let bx1 = ((x + w as i32).max(0) as usize).min(cx1);
        let by1 = ((y + h as i32).max(0) as usize).min(cy1);
        if bx0 >= bx1 || by0 >= by1 {
            return;
        }
        let bbox = (bx0, by0, bx1, by1);
        if !self.recording {
            let Canvas { pixels, width, .. } = self;
            blend_mask(pixels, *width, x, y, w, mask, c, bbox);
            return;
        }
        let at = self.damage.masks.len() as u32;
        self.damage.masks.extend_from_slice(&mask[..w * h]);
        self.emit(Op::Mask { x, y, w: w as u32, h: h as u32, at, c }, bbox);
    }
}

/// Exécute une primitive dans `clip` (inclus dans `bbox`, sa boîte complète).
fn draw_op(
    pixels: &mut [u32],
    width: usize,
    raster: &mut Rasterizer,
    segs: &[Seg],
    masks: &[u8],
    op: &Op,
    bbox: PxBox,
    clip: PxBox,
) {
    let (x0, y0, x1, y1) = clip;
    match *op {
        Op::Clear(c) => {
            let v = c.rgb_u32();
            for py in y0..y1 {
                pixels[py * width + x0..py * width + x1].fill(v);
            }
        }
        Op::Fill { r, radius, top, bottom } => {
            for py in y0..y1 {
                let cy = py as f32 + 0.5;
                let c = if top == bottom { top } else { top.mix(bottom, ((cy - r.y) / r.h).clamp(0.0, 1.0)) };
                if c.a == 0 {
                    continue;
                }
                let row = &mut pixels[py * width..(py + 1) * width];
                let cov_y = overlap(cy, r.y, r.bottom());
                let corner_row = radius > 0.5 && (cy < r.y + radius || cy > r.bottom() - radius);
                // Zone droite : pixels entièrement dans la largeur et hors des coins ;
                // seuls les bords et les coins demandent un calcul par pixel.
                let (lo, hi) = if corner_row {
                    (r.x + radius - 0.5, r.right() - radius + 0.5)
                } else {
                    (r.x, r.right())
                };
                let ix0 = (lo.ceil().max(0.0) as usize).clamp(x0, x1);
                let ix1 = (hi.floor().max(0.0) as usize).clamp(ix0, x1);
                for px in (x0..ix0).chain(ix1..x1) {
                    blend(&mut row[px], c, rrect_cov(&r, radius, px as f32 + 0.5, cy));
                }
                fill_span(&mut row[ix0..ix1], c, cov_y);
            }
        }
        Op::Stroke { r, radius, width: sw, c } => {
            let inner = r.inset(sw);
            let inner_radius = (radius - sw).max(0.0);
            // Zone droite commune au bord extérieur et intérieur : la couverture n'y
            // dépend que de la ligne.
            let lo = (r.x + radius).max(inner.x + inner_radius);
            let hi = (r.right() - radius).min(inner.right() - inner_radius);
            let sx0 = ((lo - 0.5).ceil().max(0.0) as usize).clamp(x0, x1);
            let sx1 = ((hi + 0.5).floor().max(0.0) as usize).clamp(sx0, x1);
            for py in y0..y1 {
                let cy = py as f32 + 0.5;
                let row = &mut pixels[py * width..(py + 1) * width];
                for px in (x0..sx0).chain(sx1..x1) {
                    let cx = px as f32 + 0.5;
                    let cov = rrect_cov(&r, radius, cx, cy) - rrect_cov(&inner, inner_radius, cx, cy);
                    if cov > 0.0 {
                        blend(&mut row[px], c, cov);
                    }
                }
                let inner_y = if inner.w > 0.0 { overlap(cy, inner.y, inner.bottom()) } else { 0.0 };
                let cov = overlap(cy, r.y, r.bottom()) - inner_y;
                if cov > 0.0 {
                    fill_span(&mut row[sx0..sx1], c, cov);
                }
            }
        }
        Op::Path { segs: (s0, s1), k, c } => {
            // Rastérisé sur toute sa boîte (mêmes calculs qu'une image complète),
            // mélangé seulement dans la découpe.
            let (bx0, by0, bx1, by1) = bbox;
            raster.reset(bx1 - bx0, by1 - by0);
            rasterize_segs(&segs[s0 as usize..s1 as usize], raster, k, -(bx0 as f32), -(by0 as f32));
            raster.for_each_coverage_in(x0 - bx0, y0 - by0, x1 - bx0, y1 - by0, |x, y, cov| {
                blend(&mut pixels[(by0 + y) * width + bx0 + x], c, cov)
            });
        }
        Op::Mask { x, y, w, h, at, c } => {
            let mask = &masks[at as usize..at as usize + (w * h) as usize];
            blend_mask(pixels, width, x, y, w as usize, mask, c, clip);
        }
    }
}

/// Mélange un masque 8 bits posé en (`x`, `y`), limité à `clip`.
fn blend_mask(pixels: &mut [u32], width: usize, x: i32, y: i32, w: usize, mask: &[u8], c: Color, clip: PxBox) {
    let (x0, y0, x1, y1) = clip;
    for py in y0..y1 {
        let my = (py as i32 - y) as usize;
        let row = &mut pixels[py * width..(py + 1) * width];
        let src = &mask[my * w..(my + 1) * w];
        for px in x0..x1 {
            let m = src[(px as i32 - x) as usize];
            if m != 0 {
                blend(&mut row[px], c, m as f32 / 255.0);
            }
        }
    }
}

/// Remplit une portion de ligne avec une couverture constante.
fn fill_span(span: &mut [u32], c: Color, coverage: f32) {
    if coverage >= 1.0 && c.a == 255 {
        span.fill(c.rgb_u32());
    } else if coverage > 0.0 {
        for dst in span {
            blend(dst, c, coverage);
        }
    }
}

fn disc<const N: usize>(center: Pt, r: f32) -> [Pt; N] {
    std::array::from_fn(|i| {
        let a = TAU * i as f32 / N as f32;
        (center.0 + a.cos() * r, center.1 + a.sin() * r)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coverage_sum(c: &Canvas) -> f32 {
        c.pixels().iter().map(|&p| (p & 0xff) as f32 / 255.0).sum()
    }

    #[test]
    fn rect_area_matches_with_fractional_edges() {
        let mut c = Canvas::new(40, 40);
        c.fill_rect(Rect::new(5.25, 5.5, 10.5, 20.0), Color::hex(0xffffff));
        assert!((coverage_sum(&c) - 210.0).abs() < 1.0);
    }

    #[test]
    fn rounded_rect_loses_corner_area() {
        let mut c = Canvas::new(40, 40);
        c.fill_rrect(Rect::new(0.0, 0.0, 40.0, 40.0), 10.0, Color::hex(0xffffff));
        let expected = 1600.0 - (4.0 - std::f32::consts::PI) * 100.0;
        assert!((coverage_sum(&c) - expected).abs() < 3.0, "{}", coverage_sum(&c));
    }

    #[test]
    fn theme_style_remaps_corners_but_keeps_circles() {
        let fill = |radii: [f32; 3], r: Rect, radius: f32| {
            let mut c = Canvas::new(40, 40);
            c.set_style(radii, 1.0);
            c.fill_rrect(r, radius, Color::hex(0xffffff));
            coverage_sum(&c)
        };
        let square = Rect::new(0.0, 0.0, 40.0, 40.0);
        // Rayons par défaut : rien ne change.
        assert_eq!(fill(DEFAULT_RADII, square, 10.0), fill([4.0, 8.0, 16.0], square, 10.0));
        // Coins carrés : la surface entière.
        assert!((fill([0.0, 0.0, 0.0], square, 10.0) - 1600.0).abs() < 1.0);
        // Un cercle reste un cercle.
        let disc = Rect::new(10.0, 10.0, 20.0, 20.0);
        assert_eq!(fill([0.0, 0.0, 0.0], disc, 10.0), fill(DEFAULT_RADII, disc, 10.0));
        // Contours plus épais.
        let mut c = Canvas::new(40, 40);
        c.set_style(DEFAULT_RADII, 2.0);
        c.stroke_rrect(square, 0.0, 1.0, Color::hex(0xffffff));
        assert!((coverage_sum(&c) - 304.0).abs() < 1.0);
    }

    #[test]
    fn stroke_is_a_ring() {
        let mut c = Canvas::new(40, 40);
        c.stroke_rrect(Rect::new(0.0, 0.0, 40.0, 40.0), 0.0, 1.0, Color::hex(0xffffff));
        assert!((coverage_sum(&c) - 156.0).abs() < 1.0);
        assert_eq!(c.pixels()[20 * 40 + 20], 0);
    }

    #[test]
    fn path_and_clip() {
        let mut c = Canvas::new(20, 20);
        c.set_clip(Some(Rect::new(0.0, 0.0, 10.0, 20.0)));
        let mut p = Path::new();
        p.polygon(&[(0.0, 0.0), (20.0, 0.0), (20.0, 20.0), (0.0, 20.0)]);
        c.fill_path(&p, Color::hex(0xffffff));
        assert!((coverage_sum(&c) - 200.0).abs() < 1e-2);
    }

    #[test]
    fn overlapping_strokes_do_not_cancel() {
        let mut c = Canvas::new(20, 20);
        c.stroke_polyline(&[(2.0, 10.0), (18.0, 10.0), (2.0, 10.0)], 4.0, Color::hex(0xffffff));
        assert_eq!(c.pixels()[10 * 20 + 10] & 0xff, 0xff);
    }

    #[test]
    fn scale_multiplies_covered_area() {
        let mut c = Canvas::new(80, 80);
        c.set_scale(2.0);
        assert_eq!(c.logical_size(), (40.0, 40.0));
        c.fill_rect(Rect::new(5.25, 5.5, 10.5, 20.0), Color::hex(0xffffff));
        assert!((coverage_sum(&c) - 210.0 * 4.0).abs() < 2.0, "{}", coverage_sum(&c));
        c.clear(Color::hex(0));
        c.stroke_rrect(Rect::new(0.0, 0.0, 40.0, 40.0), 0.0, 1.0, Color::hex(0xffffff));
        assert!((coverage_sum(&c) - 4.0 * 156.0).abs() < 2.0, "{}", coverage_sum(&c));
    }

    #[test]
    fn clip_rect_is_scaled_to_whole_physical_pixels() {
        let mut c = Canvas::new(100, 60);
        c.set_scale(1.5);
        assert_eq!(c.clip_pixels(), (0, 0, 100, 60));
        let r = Rect::new(1.25, 2.0, 3.0, 4.0);
        c.set_clip(Some(r));
        // 1.875 → 1 ; 6.375 → 7 ; 3 → 3 ; 9 → 9.
        assert_eq!(c.clip_pixels(), (1, 3, 7, 9));
        assert_eq!(c.clip_rect(), r);
        // Une découpe imbriquée puis restaurée retrouve exactement les mêmes pixels.
        let prev = c.push_clip(Rect::new(2.0, 0.0, 50.0, 50.0));
        assert_eq!(c.clip_pixels(), (3, 3, 7, 9));
        c.set_clip(Some(prev));
        assert_eq!(c.clip_pixels(), (1, 3, 7, 9));
        // Hors du canvas : bornée au tampon physique.
        c.set_clip(Some(Rect::new(-10.0, -10.0, 1000.0, 1000.0)));
        assert_eq!(c.clip_pixels(), (0, 0, 100, 60));
        // Changer d'échelle remet la découpe à tout le canvas, en logique.
        c.set_scale(2.0);
        assert_eq!(c.clip_rect(), Rect::new(0.0, 0.0, 50.0, 30.0));
    }

    #[test]
    fn scaled_drawing_matches_drawing_at_physical_coordinates() {
        let white = Color::hex(0xffffff);
        let (mut a, mut b) = (Canvas::new(64, 64), Canvas::new(64, 64));
        a.set_scale(2.0);
        a.fill_rrect(Rect::new(3.0, 4.0, 20.0, 12.0), 4.0, white);
        a.stroke_polyline(&[(2.0, 28.0), (30.0, 20.0)], 1.5, white);
        b.fill_rrect(Rect::new(6.0, 8.0, 40.0, 24.0), 8.0, white);
        b.stroke_polyline(&[(4.0, 56.0), (60.0, 40.0)], 3.0, white);
        assert_eq!(a.pixels(), b.pixels());
    }

    /// Une scène dont seuls quelques éléments bougent d'une image à l'autre.
    fn scene(c: &mut Canvas, t: f32) {
        let (w, h) = c.logical_size();
        c.set_clip(None);
        c.clear(Color::hex(0x101418));
        c.fill_rrect_vgrad(Rect::new(10.0, 10.0, w - 20.0, 60.0), 6.0, Color::hex(0x203040), Color::hex(0x102030));
        c.stroke_rrect(Rect::new(10.0, 80.0, w - 20.0, h - 90.0), 8.0, 1.5, Color::hex(0x40a060));
        let prev = c.push_clip(Rect::new(20.0, 90.0, w - 40.0, h - 110.0));
        c.stroke_polyline(&[(20.0, 150.0), (120.0, 100.0), (200.0, 170.0), (300.0, 120.0)], 2.0, Color::hex(0x30c0b0));
        // Tête de lecture et VU-mètre animés.
        c.fill_rect(Rect::new(30.0 + t * 7.3, 90.0, 2.0, h - 110.0), Color::hex(0x40e0d0).alpha(0.8));
        c.fill_rrect(Rect::new(w - 40.0, h - 30.0 - t * 3.0, 8.0, t * 3.0 + 1.0), 2.0, Color::hex(0x40ff70));
        c.set_clip(Some(prev));
        c.stroke_arc(60.0, 40.0, 14.0, 2.0, 2.0 + t * 0.2, 3.0, Color::hex(0x80ff80));
        let mut m = [0u8; 6 * 5];
        for (i, v) in m.iter_mut().enumerate() {
            *v = (i * 9) as u8;
        }
        c.draw_mask(200, 30, 6, 5, &m, Color::hex(0xffffff));
    }

    #[test]
    fn tracked_frames_match_full_redraws() {
        for scale in [1.0, 1.5, 2.0] {
            let (pw, ph) = ((320.0 * scale) as usize, (240.0 * scale) as usize);
            let mut tracked = Canvas::new(pw, ph);
            tracked.set_scale(scale);
            for frame in 0..12 {
                let t = (frame / 2) as f32;
                tracked.begin_frame();
                scene(&mut tracked, t);
                let dirty = tracked.end_frame().to_vec();
                let mut full = Canvas::new(pw, ph);
                full.set_scale(scale);
                scene(&mut full, t);
                assert_eq!(tracked.pixels(), full.pixels(), "image {frame} à l'échelle {scale}");
                if frame == 0 {
                    assert_eq!(dirty.iter().map(|d| (d.2 - d.0) * (d.3 - d.1)).sum::<usize>(), pw * ph);
                } else if frame % 2 == 1 {
                    // Rien n'a bougé : rien à redessiner ni à présenter.
                    assert!(dirty.is_empty(), "{dirty:?}");
                } else {
                    let area: usize = dirty.iter().map(|d| (d.2 - d.0) * (d.3 - d.1)).sum();
                    assert!(area > 0 && area < pw * ph / 3, "{area} / {}", pw * ph);
                }
            }
        }
    }

    #[test]
    fn invalidation_redraws_everything() {
        let mut c = Canvas::new(100, 70);
        for _ in 0..2 {
            c.begin_frame();
            scene(&mut c, 1.0);
            c.end_frame();
        }
        c.invalidate();
        c.begin_frame();
        scene(&mut c, 1.0);
        let area: usize = c.end_frame().iter().map(|d| (d.2 - d.0) * (d.3 - d.1)).sum();
        assert_eq!(area, 100 * 70);
        // Les zones sont disjointes et couvrent le canvas sans déborder.
        c.resize(130, 90);
        c.begin_frame();
        scene(&mut c, 2.0);
        let dirty = c.end_frame().to_vec();
        assert!(dirty.iter().all(|d| d.2 <= 130 && d.3 <= 90));
        assert_eq!(dirty.iter().map(|d| (d.2 - d.0) * (d.3 - d.1)).sum::<usize>(), 130 * 90);
    }
}
