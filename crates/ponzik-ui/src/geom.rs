/// Rectangle en pixels logiques (flottants).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }

    pub fn center(&self) -> (f32, f32) {
        (self.x + self.w * 0.5, self.y + self.h * 0.5)
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }

    /// Rétrécit de `d` de chaque côté.
    pub fn inset(&self, d: f32) -> Rect {
        Rect::new(self.x + d, self.y + d, (self.w - 2.0 * d).max(0.0), (self.h - 2.0 * d).max(0.0))
    }

    /// Rétrécit différemment en horizontal et en vertical.
    pub fn inset_xy(&self, dx: f32, dy: f32) -> Rect {
        Rect::new(self.x + dx, self.y + dy, (self.w - 2.0 * dx).max(0.0), (self.h - 2.0 * dy).max(0.0))
    }

    /// Mise à l'échelle autour du centre.
    pub fn scale(&self, s: f32) -> Rect {
        let (cx, cy) = self.center();
        Rect::new(cx - self.w * s * 0.5, cy - self.h * s * 0.5, self.w * s, self.h * s)
    }

    pub fn translate(&self, dx: f32, dy: f32) -> Rect {
        Rect::new(self.x + dx, self.y + dy, self.w, self.h)
    }

    /// Coupe une bande de hauteur `h` en haut ; renvoie (bande, reste).
    pub fn take_top(&self, h: f32) -> (Rect, Rect) {
        let h = h.min(self.h);
        (Rect::new(self.x, self.y, self.w, h), Rect::new(self.x, self.y + h, self.w, self.h - h))
    }

    /// Coupe une bande de largeur `w` à gauche ; renvoie (bande, reste).
    pub fn take_left(&self, w: f32) -> (Rect, Rect) {
        let w = w.min(self.w);
        (Rect::new(self.x, self.y, w, self.h), Rect::new(self.x + w, self.y, self.w - w, self.h))
    }

    /// Coupe une bande de largeur `w` à droite ; renvoie (bande, reste).
    pub fn take_right(&self, w: f32) -> (Rect, Rect) {
        let w = w.min(self.w);
        (Rect::new(self.right() - w, self.y, w, self.h), Rect::new(self.x, self.y, self.w - w, self.h))
    }

    pub fn intersect(&self, o: &Rect) -> Rect {
        let x = self.x.max(o.x);
        let y = self.y.max(o.y);
        Rect::new(x, y, (self.right().min(o.right()) - x).max(0.0), (self.bottom().min(o.bottom()) - y).max(0.0))
    }
}
