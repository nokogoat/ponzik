/// Échantillon audio décodé en mémoire, partagé entre instruments.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sample {
    pub name: String,
    pub sample_rate: f32,
    pub left: Vec<f32>,
    /// Identique à `left` pour une source mono.
    pub right: Vec<f32>,
}

impl Sample {
    pub fn len(&self) -> usize {
        self.left.len()
    }

    pub fn is_empty(&self) -> bool {
        self.left.is_empty()
    }

    pub fn duration(&self) -> f32 {
        self.len() as f32 / self.sample_rate.max(1.0)
    }
}
