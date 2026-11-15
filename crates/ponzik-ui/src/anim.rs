use std::collections::HashMap;

/// Valeurs animées par identifiant : chacune rejoint sa cible par approche
/// exponentielle (une décélération douce, proche de `cubic-bezier(0.22, 1, 0.36, 1)`).
#[derive(Default)]
pub struct Anim {
    values: HashMap<u64, f32>,
    dt: f32,
    moving: bool,
}

impl Anim {
    pub fn new() -> Self {
        Self::default()
    }

    /// À appeler une fois par image avec la durée écoulée.
    pub fn begin_frame(&mut self, dt: f32) {
        self.dt = dt.clamp(0.0, 0.1);
        self.moving = false;
    }

    /// Fait tendre la valeur `id` vers `target` ; `tau` = constante de temps en secondes
    /// (≈ 95 % du chemin parcouru en 3 × `tau`).
    pub fn to(&mut self, id: u64, target: f32, tau: f32) -> f32 {
        let dt = self.dt;
        let v = self.values.entry(id).or_insert(target);
        let k = 1.0 - (-dt / tau.max(1e-4)).exp();
        *v += (target - *v) * k;
        if (target - *v).abs() < 1e-3 {
            *v = target;
        } else {
            self.moving = true;
        }
        *v
    }

    /// Force une valeur (déclenchement d'un flash, par exemple).
    pub fn set(&mut self, id: u64, value: f32) {
        self.values.insert(id, value);
        self.moving = true;
    }

    pub fn get(&self, id: u64) -> Option<f32> {
        self.values.get(&id).copied()
    }

    /// Vrai si une valeur n'a pas encore atteint sa cible : il faut redessiner.
    pub fn is_moving(&self) -> bool {
        self.moving
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converges_and_settles() {
        let mut a = Anim::new();
        a.begin_frame(1.0 / 60.0);
        assert_eq!(a.to(1, 0.0, 0.05), 0.0);
        let mut v = 0.0;
        for _ in 0..9 {
            a.begin_frame(1.0 / 60.0);
            v = a.to(1, 1.0, 0.05);
        }
        assert!(v > 0.9 && v < 1.0);
        for _ in 0..60 {
            a.begin_frame(1.0 / 60.0);
            a.to(1, 1.0, 0.05);
        }
        assert!(!a.is_moving());
    }
}
