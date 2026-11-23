use std::collections::HashMap;

/// Valeurs animées par identifiant : chacune rejoint sa cible par approche
/// exponentielle (une décélération douce, proche de `cubic-bezier(0.22, 1, 0.36, 1)`).
#[derive(Default)]
pub struct Anim {
    values: HashMap<u64, f32>,
    dt: f32,
    moving: bool,
    /// Une valeur suivie par [`Anim::to_ambient`] bouge encore.
    ambient: bool,
    /// Vitesse des animations choisie par le thème : 1 normale, 2 deux fois plus
    /// vite, 0 aucune (chaque valeur saute à sa cible).
    speed: Option<f32>,
}

impl Anim {
    pub fn new() -> Self {
        Self::default()
    }

    /// À appeler une fois par image avec la durée écoulée.
    pub fn begin_frame(&mut self, dt: f32) {
        self.dt = dt.clamp(0.0, 0.1);
        self.moving = false;
        self.ambient = false;
    }

    /// Fait tendre la valeur `id` vers `target` ; `tau` = constante de temps en secondes
    /// (≈ 95 % du chemin parcouru en 3 × `tau`).
    pub fn to(&mut self, id: u64, target: f32, tau: f32) -> f32 {
        let dt = self.dt;
        let speed = self.speed.unwrap_or(1.0);
        let v = self.values.entry(id).or_insert(target);
        let k = if speed <= 0.0 { 1.0 } else { 1.0 - (-dt / (tau / speed).max(1e-4)).exp() };
        *v += (target - *v) * k;
        if (target - *v).abs() < 1e-3 {
            *v = target;
        } else {
            self.moving = true;
        }
        *v
    }

    /// Comme [`Anim::to`], pour une animation d'ambiance qui suit la lecture
    /// (LED de temps, éclats rythmiques) : elle demande des images, mais pas la
    /// cadence pleine (voir [`Anim::is_ambient`]).
    pub fn to_ambient(&mut self, id: u64, target: f32, tau: f32) -> f32 {
        let moving = std::mem::take(&mut self.moving);
        let v = self.to(id, target, tau);
        self.ambient |= self.moving;
        self.moving = moving;
        v
    }

    /// Force une valeur (déclenchement d'un flash, par exemple). Redonner la
    /// même valeur ne demande pas d'image : rien n'a bougé.
    pub fn set(&mut self, id: u64, value: f32) {
        if self.values.insert(id, value) != Some(value) {
            self.moving = true;
        }
    }

    /// Vitesse des animations (voir le champ) : 1 par défaut, 0 pour aucune.
    pub fn set_speed(&mut self, speed: f32) {
        let s = if speed.is_finite() { speed.clamp(0.0, 4.0) } else { 1.0 };
        self.speed = (s != 1.0).then_some(s);
    }

    pub fn get(&self, id: u64) -> Option<f32> {
        self.values.get(&id).copied()
    }

    /// Vrai si une valeur n'a pas encore atteint sa cible : il faut redessiner.
    pub fn is_moving(&self) -> bool {
        self.moving
    }

    /// Vrai si seule une animation d'ambiance bouge encore : une cadence réduite suffit.
    pub fn is_ambient(&self) -> bool {
        self.ambient
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

    #[test]
    fn speed_zero_jumps_and_speed_two_is_faster() {
        let mut a = Anim::new();
        a.set_speed(0.0);
        a.begin_frame(1.0 / 60.0);
        a.to(1, 0.0, 0.05);
        a.begin_frame(1.0 / 60.0);
        assert_eq!(a.to(1, 1.0, 0.05), 1.0);
        let step = |speed: f32| {
            let mut a = Anim::new();
            a.set_speed(speed);
            a.begin_frame(1.0 / 60.0);
            a.to(1, 0.0, 0.05);
            a.begin_frame(1.0 / 60.0);
            a.to(1, 1.0, 0.05)
        };
        assert!(step(2.0) > step(1.0));
    }

    #[test]
    fn ambient_values_do_not_count_as_moving() {
        let mut a = Anim::new();
        a.begin_frame(1.0 / 60.0);
        a.to_ambient(1, 0.0, 0.1);
        a.begin_frame(1.0 / 60.0);
        a.to_ambient(1, 1.0, 0.1);
        assert!(a.is_ambient() && !a.is_moving());
        a.to(2, 0.0, 0.1);
        a.begin_frame(1.0 / 60.0);
        a.to(2, 1.0, 0.1);
        a.to_ambient(1, 1.0, 0.1);
        assert!(a.is_moving() && a.is_ambient());
        // La même valeur redonnée ne demande pas d'image.
        a.begin_frame(1.0 / 60.0);
        a.set(3, 0.5);
        assert!(a.is_moving());
        a.begin_frame(1.0 / 60.0);
        a.set(3, 0.5);
        assert!(!a.is_moving());
    }
}
