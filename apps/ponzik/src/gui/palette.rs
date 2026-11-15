//! Palette « touche-à-tout » (Ctrl+K) : toutes les actions de Ponzik, trouvées
//! en quelques lettres.

use ponzik_engine::registry;
use ponzik_ui::{Canvas, Color, Rect};
use ponzik_window::Key;

use super::Input;
use super::app::{Action, App, Popup, Rename, View};
use super::theme::*;
use super::widgets::wid;

const ROW_H: f32 = 44.0;
const VISIBLE: usize = 9;

pub struct Item {
    pub title: String,
    pub category: &'static str,
    pub shortcut: &'static str,
    pub action: Action,
}

/// Minuscules sans accents, pour chercher « reverbe » et trouver « Réverbe ».
pub fn normalize(s: &str) -> String {
    s.chars()
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'à' | 'â' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'î' | 'ï' => 'i',
            'ô' | 'ö' => 'o',
            'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            c => c,
        })
        .collect()
}

/// Score de correspondance floue : les lettres de la requête dans l'ordre, avec
/// un bonus pour les suites contiguës et les débuts de mots. `None` si absent.
pub fn fuzzy_score(query: &str, text: &str) -> Option<i32> {
    let q: Vec<char> = normalize(query).chars().filter(|c| !c.is_whitespace()).collect();
    if q.is_empty() {
        return Some(0);
    }
    let t: Vec<char> = normalize(text).chars().collect();
    let mut score = 0;
    let mut qi = 0;
    let mut last: Option<usize> = None;
    for (i, &c) in t.iter().enumerate() {
        if qi < q.len() && c == q[qi] {
            score += 10;
            if last == Some(i.wrapping_sub(1)) {
                score += 15;
            }
            if i == 0 || !t[i - 1].is_alphanumeric() {
                score += 20;
            }
            last = Some(i);
            qi += 1;
        }
    }
    (qi == q.len()).then(|| score - t.len() as i32 / 4)
}

impl App {
    fn palette_items(&self) -> Vec<Item> {
        let mut v = Vec::new();
        let mut add = |title: String, category: &'static str, shortcut: &'static str, action: Action| {
            v.push(Item { title, category, shortcut, action });
        };
        add("Lecture / arrêt".into(), "Transport", "Espace", Action::PlayStop);
        add("Basculer pattern / morceau".into(), "Transport", "L", Action::ToggleMode);
        add("Métronome".into(), "Transport", "Ctrl M", Action::ToggleMetronome);
        add("Armer l'enregistrement".into(), "Transport", "R", Action::ToggleRecord);
        add("Clavier d'ordinateur musical".into(), "Transport", "Ctrl T", Action::ToggleTyping);
        for (view, key) in [(View::Playlist, "F5"), (View::Rack, "F6"), (View::Piano, "F7"), (View::Mixer, "F9")] {
            add(format!("Aller à : {}", view.label()), "Vue", key, Action::SetView(view));
        }
        add("Afficher ou masquer le navigateur".into(), "Vue", "Alt F8", Action::ToggleSidebar);
        add("Afficher ou masquer les appareils".into(), "Vue", "", Action::TogglePanel);
        add("Nouveau pattern".into(), "Pattern", "F4", Action::NewPattern);
        add("Dupliquer le pattern".into(), "Pattern", "", Action::DuplicatePattern);
        let cur = self.project.current_pattern;
        add(
            "Renommer le pattern".into(),
            "Pattern",
            "",
            Action::Popup(Popup::Rename { target: Rename::Pattern(cur), text: self.project.patterns[cur].name.clone() }),
        );
        for (i, p) in self.project.patterns.iter().enumerate() {
            add(format!("Pattern : {}", p.name), "Pattern", "", Action::SelectPattern(i));
        }
        for (i, c) in self.project.channels.iter().enumerate() {
            add(format!("Canal : {}", c.name), "Canal", "", Action::FocusChannel(i));
        }
        for k in registry::instruments() {
            add(format!("Ajouter {}", k.name), "Instrument", "", Action::AddInstrument(k.id, None));
        }
        let track = self.focused_track();
        for k in registry::effects() {
            add(format!("{} sur {}", k.name, self.project.tracks[track].name), "Effet", "", Action::AddEffect(track, k.id));
        }
        add("Enregistrer".into(), "Fichier", "Ctrl S", Action::Save);
        add("Ouvrir un projet".into(), "Fichier", "Ctrl O", Action::OpenMenu);
        for path in super::compat::list_projects().into_iter().take(6) {
            let name = path.file_stem().map_or(String::new(), |s| s.to_string_lossy().into_owned());
            add(format!("Ouvrir : {name}"), "Récent", "", Action::Open(path));
        }
        add("Nouveau projet".into(), "Fichier", "Ctrl N", Action::NewProject);
        add("Exporter en WAV".into(), "Fichier", "Ctrl R", Action::Export);
        add("Exporter les pistes séparées (stems)".into(), "Fichier", "", Action::ExportStems);
        add("Importer un fichier MIDI".into(), "Fichier", "", Action::ImportMidiMenu);
        add("Exporter le pattern en MIDI".into(), "Fichier", "", Action::ExportMidi);
        add("Enregistrer sous".into(), "Fichier", "Ctrl Maj S", Action::SaveAs);
        add(
            "Renommer le projet".into(),
            "Fichier",
            "",
            Action::Popup(Popup::Rename { target: Rename::Project, text: self.project.name.clone() }),
        );
        add("Annuler".into(), "Édition", "Ctrl Z", Action::Undo);
        add("Rétablir".into(), "Édition", "Ctrl Y", Action::Redo);
        add("Démo : groove house".into(), "Démo", "", Action::LoadDemo(0));
        add("Démo : Amen rave (breakcore)".into(), "Démo", "", Action::LoadDemo(1));
        for (i, th) in THEMES.iter().enumerate() {
            add(format!("Thème : {}", th.name), "Thème", "", Action::SetTheme(i));
        }
        v
    }

    /// Dessine la palette et gère ses touches ; renvoie l'action choisie et la
    /// palette mise à jour (ou `None` si elle se ferme).
    pub(super) fn palette(&mut self, c: &mut Canvas, input: &Input, screen: Rect, mut query: String, mut selected: usize) -> (Option<Action>, Option<Popup>) {
        let t = self.theme;
        let open = self.anim.to(wid("popup", 0, 0), 1.0, T_HOVER);
        query.push_str(&input.text);

        let all = self.palette_items();
        let mut found: Vec<(i32, usize)> = all
            .iter()
            .enumerate()
            .filter_map(|(i, it)| fuzzy_score(&query, &format!("{} {}", it.title, it.category)).map(|s| (s, i)))
            .collect();
        if !query.trim().is_empty() {
            found.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        }
        let count = found.len();

        let mut run = None;
        let mut close = false;
        for &(key, _) in &input.keys {
            match key {
                Key::Escape => close = true,
                Key::Backspace => {
                    query.pop();
                    selected = 0;
                }
                Key::Down => selected = (selected + 1).min(count.saturating_sub(1)),
                Key::Up => selected = selected.saturating_sub(1),
                Key::PageDown => selected = (selected + VISIBLE).min(count.saturating_sub(1)),
                Key::PageUp => selected = selected.saturating_sub(VISIBLE),
                Key::Enter => {
                    if let Some(&(_, i)) = found.get(selected) {
                        run = Some(all[i].action.clone());
                    }
                }
                _ => {}
            }
        }
        if !input.text.is_empty() {
            selected = 0;
        }
        selected = selected.min(count.saturating_sub(1));

        // Fond assombri, puis la palette en haut de l'écran.
        c.fill_rect(screen, Color::rgba(0, 0, 0, (120.0 * open) as u8));
        let w = 620.0f32.min(screen.w - 48.0);
        let rows = count.clamp(1, VISIBLE);
        let r = Rect::new((screen.w - w) * 0.5, screen.h * 0.14 + (1.0 - open) * 10.0, w, 64.0 + rows as f32 * ROW_H + 12.0);
        c.fill_rrect(r.translate(0.0, 8.0).inset(-4.0), 20.0, Color::rgba(0, 0, 0, (80.0 * open) as u8));
        c.fill_rrect(r, 18.0, t.panel);
        c.stroke_rrect(r, 18.0, 1.0, t.hairline.alpha(1.8));

        let field = Rect::new(r.x + 12.0, r.y + 12.0, r.w - 24.0, 44.0);
        c.fill_rrect(field, 12.0, t.well);
        // Loupe.
        c.stroke_circle(field.x + 22.0, field.y + 20.0, 6.5, 1.6, t.muted);
        c.stroke_polyline(&[(field.x + 27.0, field.y + 25.0), (field.x + 31.0, field.y + 29.0)], 1.6, t.muted);
        let shown = if query.is_empty() { "Rechercher une action, un instrument, un effet…".to_string() } else { query.clone() };
        let ink = if query.is_empty() { t.muted } else { t.text };
        let tw = self.text(c, self.f.sans, 15.0, field.x + 42.0, field.y + 22.0, &shown, ink);
        if (self.time * 2.0).fract() < 0.6 {
            let cx = if query.is_empty() { field.x + 42.0 } else { field.x + 43.0 + tw };
            c.fill_rect(Rect::new(cx, field.y + 12.0, 1.5, 20.0), t.select);
        }
        self.anim.set(wid("caret", 0, 0), self.time);
        self.text_right(c, self.f.mono, 11.0, field.right() - 12.0, field.y + 22.0, &format!("{count}"), t.muted);

        // Résultats, avec défilement qui suit la sélection.
        let first = selected.saturating_sub(VISIBLE - 1).min(count.saturating_sub(VISIBLE));
        let list = Rect::new(r.x + 6.0, field.bottom() + 8.0, r.w - 12.0, rows as f32 * ROW_H);
        if count == 0 {
            self.text(c, self.f.sans, 13.0, list.x + 16.0, list.y + ROW_H * 0.5, "Rien de trouvé", t.muted);
        }
        for (k, &(_, i)) in found.iter().enumerate().skip(first).take(VISIBLE) {
            let row = Rect::new(list.x, list.y + (k - first) as f32 * ROW_H, list.w, ROW_H - 4.0);
            let hov = input.inside && row.contains(input.x, input.y);
            if hov && input.pressed {
                run = Some(all[i].action.clone());
            }
            let sel = k == selected;
            let sv = self.anim.to(wid("pal-sel", k, 0), sel as u8 as f32, T_PRESS);
            if sv > 0.01 || hov {
                c.fill_rrect(row, 10.0, t.raised.mix(t.raised_hi, 0.4).alpha(sv.max(hov as u8 as f32 * 0.5)));
            }
            if sel {
                c.fill_rrect(Rect::new(row.x + 4.0, row.y + 12.0, 3.0, row.h - 24.0), 1.5, t.accent);
            }
            let item = &all[i];
            let right = row.right() - 12.0;
            let mut x = right;
            if !item.shortcut.is_empty() {
                let kw = self.fonts.measure(self.f.mono, 11.0, item.shortcut) + 14.0;
                let cap = Rect::new(x - kw, row.y + 10.0, kw, 20.0);
                c.fill_rrect(cap, 5.0, t.raised_hi);
                c.fill_rect(Rect::new(cap.x + 3.0, cap.bottom() - 1.0, cap.w - 6.0, 1.0), Color::rgba(0, 0, 0, 90));
                self.text_center(c, self.f.mono, 11.0, cap.x + cap.w * 0.5, cap.y + 10.0, item.shortcut, t.text2);
                x = cap.x - 12.0;
            }
            let cat_w = self.fonts.measure(self.f.sans, 11.0, item.category);
            self.text(c, self.f.sans, 11.0, x - cat_w, row.y + 20.0, item.category, t.muted);
            let title = item.title.clone();
            self.text_fit(c, self.f.sans_med, 14.0, row.x + 18.0, row.y + 20.0, &title, x - cat_w - row.x - 34.0, t.text);
        }
        if input.pressed && !r.contains(input.x, input.y) {
            close = true;
        }
        if run.is_some() || close {
            (run, None)
        } else {
            (None, Some(Popup::Palette { query, selected }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_matching() {
        assert!(fuzzy_score("rev", "Réverbe").is_some());
        assert!(fuzzy_score("reverbe", "Ajouter Réverbe").is_some());
        assert!(fuzzy_score("xyz", "Réverbe").is_none());
        let start = fuzzy_score("pl", "Aller à : Playlist").unwrap();
        let scattered = fuzzy_score("pl", "Aller à : Piano roll").unwrap();
        assert!(start > scattered, "{start} > {scattered}");
        assert_eq!(normalize("Ça Été"), "ca ete");
    }
}
