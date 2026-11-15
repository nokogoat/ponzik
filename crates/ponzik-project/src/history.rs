//! Annuler / rétablir par instantanés complets du projet (un projet pèse peu).
//!
//! Usage : appeler [`History::push`] avec l'état *avant* chaque modification.
//! Les modifications continues (glisser un bouton, peindre des notes) poussent
//! le même libellé à la suite : elles fusionnent en une seule étape tant que
//! deux poussées sont espacées de moins de [`History::window`] secondes.

use std::collections::VecDeque;

use crate::Project;

/// Nombre d'étapes gardées par défaut.
pub const DEFAULT_CAP: usize = 200;
/// Fenêtre de fusion par défaut, en secondes.
pub const DEFAULT_WINDOW: f64 = 1.0;

#[derive(Clone, Debug)]
struct Entry {
    label: String,
    project: Project,
}

#[derive(Clone, Debug)]
pub struct History {
    undo: VecDeque<Entry>,
    redo: Vec<Entry>,
    cap: usize,
    /// Fenêtre de fusion des poussées de même libellé, en secondes.
    pub window: f64,
    /// Libellé et instant de la dernière poussée, tant qu'elle peut être prolongée.
    last: Option<(String, f64)>,
}

impl Default for History {
    fn default() -> Self {
        Self::new(DEFAULT_CAP)
    }
}

impl History {
    pub fn new(cap: usize) -> Self {
        Self { undo: VecDeque::new(), redo: Vec::new(), cap: cap.max(1), window: DEFAULT_WINDOW, last: None }
    }

    /// Mémorise `before`, l'état précédant une modification, à l'instant `now`
    /// (en secondes, horloge quelconque mais croissante).
    pub fn push(&mut self, label: &str, before: &Project, now: f64) {
        let coalesce = self
            .last
            .as_ref()
            .is_some_and(|(l, t)| l == label && now >= *t && now - *t <= self.window && !self.undo.is_empty());
        self.last = Some((label.to_string(), now));
        if coalesce {
            // L'étape en cours a déjà son état de départ.
            return;
        }
        self.redo.clear();
        self.undo.push_back(Entry { label: label.to_string(), project: before.clone() });
        while self.undo.len() > self.cap {
            self.undo.pop_front();
        }
    }

    /// Termine l'étape en cours : la prochaine poussée ne fusionnera pas.
    pub fn seal(&mut self) {
        self.last = None;
    }

    /// Restaure l'état précédent dans `project` ; renvoie le libellé annulé.
    pub fn undo(&mut self, project: &mut Project) -> Option<String> {
        let e = self.undo.pop_back()?;
        self.last = None;
        let current = std::mem::replace(project, e.project);
        self.redo.push(Entry { label: e.label.clone(), project: current });
        Some(e.label)
    }

    /// Rejoue la dernière étape annulée ; renvoie son libellé.
    pub fn redo(&mut self, project: &mut Project) -> Option<String> {
        let e = self.redo.pop()?;
        self.last = None;
        let current = std::mem::replace(project, e.project);
        self.undo.push_back(Entry { label: e.label.clone(), project: current });
        Some(e.label)
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Libellé de ce qu'annulerait [`History::undo`] (« Annuler … » dans un menu).
    pub fn undo_label(&self) -> Option<&str> {
        self.undo.back().map(|e| e.label.as_str())
    }

    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|e| e.label.as_str())
    }

    pub fn len(&self) -> usize {
        self.undo.len()
    }

    pub fn is_empty(&self) -> bool {
        self.undo.is_empty()
    }

    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.last = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_bpm(bpm: f32) -> Project {
        Project { bpm, ..Project::empty() }
    }

    #[test]
    fn undo_redo_round_trip() {
        let mut h = History::default();
        let mut p = with_bpm(100.0);
        assert!(!h.can_undo() && !h.can_redo());
        assert_eq!(h.undo(&mut p), None);

        h.push("Tempo", &p, 0.0);
        p.bpm = 110.0;
        h.push("Nom", &p, 10.0);
        p.name = "B".into();

        assert_eq!(h.undo_label(), Some("Nom"));
        assert_eq!(h.undo(&mut p).as_deref(), Some("Nom"));
        assert_eq!(p, with_bpm(110.0));
        assert_eq!(h.undo(&mut p).as_deref(), Some("Tempo"));
        assert_eq!(p, with_bpm(100.0));
        assert!(!h.can_undo() && h.can_redo());
        assert_eq!(h.redo_label(), Some("Tempo"));
        assert_eq!(h.redo(&mut p).as_deref(), Some("Tempo"));
        assert_eq!(p.bpm, 110.0);
        assert_eq!(h.redo(&mut p).as_deref(), Some("Nom"));
        assert_eq!(p.name, "B");
        assert_eq!(h.redo(&mut p), None);
    }

    #[test]
    fn new_edit_clears_redo() {
        let mut h = History::default();
        let mut p = with_bpm(100.0);
        h.push("Tempo", &p, 0.0);
        p.bpm = 90.0;
        h.undo(&mut p);
        assert!(h.can_redo());
        h.push("Swing", &p, 1.0);
        assert!(!h.can_redo());
    }

    #[test]
    fn coalesces_within_window() {
        let mut h = History::default();
        let mut p = with_bpm(100.0);
        for (i, t) in [0.0, 0.3, 0.6, 1.2].into_iter().enumerate() {
            h.push("Tempo", &p, t);
            p.bpm = 101.0 + i as f32;
        }
        assert_eq!(h.len(), 1, "glissement continu : une seule étape");
        // Trop tard, autre libellé, ou étape scellée : nouvelle étape.
        h.push("Tempo", &p, 5.0);
        h.push("Swing", &p, 5.1);
        h.seal();
        h.push("Swing", &p, 5.2);
        assert_eq!(h.len(), 4);
        // Après une annulation, pas de fusion avec l'étape annulée.
        h.undo(&mut p);
        h.push("Swing", &p, 5.3);
        assert_eq!(h.len(), 4);
        while h.undo(&mut p).is_some() {}
        assert_eq!(p.bpm, 100.0);
    }

    #[test]
    fn cap_drops_oldest() {
        let mut h = History::new(3);
        let mut p = with_bpm(100.0);
        for i in 0..5 {
            h.push("Tempo", &p, i as f64 * 10.0);
            p.bpm += 1.0;
        }
        assert_eq!(h.len(), 3);
        while h.undo(&mut p).is_some() {}
        assert_eq!(p.bpm, 102.0);
    }
}
