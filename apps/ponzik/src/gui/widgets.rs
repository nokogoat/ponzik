//! Widgets maison en mode immédiat : chaque appel dessine et gère son interaction.

use std::f32::consts::PI;

use ponzik_engine::{ParamInfo, Unit};
use ponzik_ui::{Canvas, Color, FontId, Path, Rect};
use ponzik_window::Cursor;

use super::Input;
use super::app::{App, Drag};
use super::theme::*;

/// Identifiant de widget stable : empreinte FNV d'un nom et de deux indices.
pub fn wid(tag: &str, a: usize, b: usize) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in tag.bytes().chain(a.to_le_bytes()).chain(b.to_le_bytes()) {
        h ^= byte as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// Infobulle : libellé (lignes suivantes en second plan) et raccourci clavier.
pub type Tip<'a> = (&'a str, &'a str);

/// Contenu d'un segment de sélecteur.
#[derive(Clone, Copy)]
enum Seg<'a> {
    Text(&'a str),
    Icon(Icon),
}

/// État des infobulles : demandée par le contrôle survolé, affichée après un délai.
#[derive(Default)]
pub struct TipState {
    want: Option<(String, String)>,
    current: Option<(String, String)>,
    since: f32,
    last_shown: f32,
    blocked: bool,
    /// Une infobulle attend son délai : l'image doit continuer de tourner.
    pub waiting: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Play,
    Pause,
    Stop,
    Plus,
    Close,
    Metronome,
    Undo,
    Redo,
    Save,
    Folder,
    Export,
    Sidebar,
    Panel,
    ChevronLeft,
    ChevronRight,
    #[allow(dead_code)]
    ChevronDown,
    Power,
    Keyboard,
    Search,
    // Vues (et modes de lecture qui leur correspondent).
    /// Grille de pas : Channel Rack, mode pattern.
    Rack,
    /// Touches et notes : piano roll.
    Piano,
    /// Clips empilés : playlist, mode morceau.
    Playlist,
    /// Grille de clips avec lecture : session.
    Session,
    /// Faders : mixer.
    Mixer,
    // Navigateur.
    Instrument,
    Effect,
    Sample,
    Plugin,
    // Pistes et canaux.
    Mute,
    Solo,
    Record,
    // Actions.
    Pencil,
    Duplicate,
    Trash,
    Swap,
    Route,
    Send,
    Reset,
    Automation,
    Preset,
    Info,
    Magnet,
    Columns,
    /// Valider.
    Check,
    /// Marches montantes : gamme du projet.
    Scale,
    /// Trois têtes de note empilées : accord.
    Chord,
    /// Dé : humaniser.
    Dice,
    /// Notes en escalier, une à une : arpégiateur.
    Arpeggio,
    /// Coups répétés de plus en plus faibles : répétition.
    Repeat,
    /// Horloge : historique des modifications.
    History,
    /// Trois traits : menu principal.
    Menu,
    /// Pastille mi-pleine : thème.
    Theme,
    /// Clé : entrée latérale d'un effet.
    Key,
    /// Fenêtre et flèche qui en sort : détacher un appareil.
    Detach,
    /// Flèche qui rentre dans le panneau du bas : réancrer.
    Dock,
    /// Trois lignes qui se rejoignent : bus.
    Bus,
    /// Deux faders liés : VCA.
    Vca,
    /// Fenêtre découpée en zones : espace de travail et réglages.
    Layout,
    /// Forme d'onde et marqueur : l'Atelier.
    Lab,
    /// Tranches et marqueurs : découper vers le Découpeur.
    Slice,
    /// Double flèche : étirer dans le temps.
    Stretch,
    /// Barres sous un plafond : normaliser.
    Normalize,
    /// Barres en dégradé : couleurs spectrales.
    Spectral,
    /// Micro : entrée audio, panneau d'enregistrement.
    Mic,
    /// Globe : langue de l'interface.
    Globe,
    /// Window with a knob and a fader: a plugin's own interface.
    PluginWindow,
}

/// Dessine une icône au trait, dans un carré de `size` centré sur (`cx`, `cy`).
pub fn draw_icon(c: &mut Canvas, icon: Icon, cx: f32, cy: f32, size: f32, col: Color) {
    let s = size / 16.0;
    let p = |x: f32, y: f32| (cx + x * s, cy + y * s);
    let rr = |x: f32, y: f32, w: f32, h: f32| Rect::new(cx + x * s, cy + y * s, w * s, h * s);
    let stroke = 1.6 * s.max(0.9);
    match icon {
        Icon::Rack => {
            // Six pas, dont trois allumés.
            for (i, on) in [true, false, true, false, true, true].into_iter().enumerate() {
                let (x, y) = (-7.0 + (i % 3) as f32 * 5.0, -5.5 + (i / 3) as f32 * 6.5);
                c.fill_rrect(rr(x, y, 4.0, 4.5), 1.2 * s, if on { col } else { col.alpha(0.38) });
            }
        }
        Icon::Piano => {
            c.stroke_rrect(rr(-7.0, -6.5, 4.5, 13.0), 1.2 * s, stroke, col);
            c.fill_rrect(rr(-0.5, -5.0, 5.0, 2.6), 1.0 * s, col);
            c.fill_rrect(rr(2.5, -1.3, 4.5, 2.6), 1.0 * s, col);
            c.fill_rrect(rr(0.5, 2.6, 4.0, 2.6), 1.0 * s, col);
        }
        Icon::Playlist => {
            c.fill_rrect(rr(-7.5, -6.0, 8.0, 3.2), 1.2 * s, col);
            c.fill_rrect(rr(-2.0, -1.6, 9.5, 3.2), 1.2 * s, col);
            c.fill_rrect(rr(-5.5, 2.8, 6.5, 3.2), 1.2 * s, col.alpha(0.55));
        }
        Icon::Session => {
            for i in 0..4 {
                let (x, y) = (-7.0 + (i % 2) as f32 * 7.5, -7.0 + (i / 2) as f32 * 7.5);
                if i == 0 {
                    c.fill_rrect(rr(x, y, 6.5, 6.5), 1.5 * s, col);
                } else {
                    c.stroke_rrect(rr(x, y, 6.5, 6.5), 1.5 * s, stroke * 0.85, col);
                }
            }
        }
        Icon::Mixer => {
            for (i, knob) in [2.0, -3.0, 0.5].into_iter().enumerate() {
                let x = -5.0 + i as f32 * 5.0;
                c.stroke_polyline(&[p(x, -7.0), p(x, 7.0)], stroke * 0.8, col.alpha(0.6));
                c.fill_rrect(rr(x - 2.2, knob - 1.4, 4.4, 2.8), 1.0 * s, col);
            }
        }
        Icon::Instrument => {
            // Une croche.
            c.fill_circle(cx - 2.5 * s, cy + 4.2 * s, 2.7 * s, col);
            c.stroke_polyline(&[p(0.0, 4.2), p(0.0, -6.5), p(5.0, -4.0), p(5.0, -1.5)], stroke, col);
        }
        Icon::Effect => {
            c.stroke_arc(cx, cy + 0.5 * s, 6.5 * s, 0.75 * PI, 2.25 * PI, stroke, col.alpha(0.45));
            c.stroke_arc(cx, cy + 0.5 * s, 6.5 * s, 0.75 * PI, 1.7 * PI, stroke, col);
            c.fill_circle(cx, cy + 0.5 * s, 3.2 * s, col);
        }
        Icon::Sample => {
            for (i, h) in [2.0, 6.0, 10.0, 13.0, 7.0, 9.0, 3.5].into_iter().enumerate() {
                let x = -6.6 + i as f32 * 2.2;
                c.stroke_polyline(&[p(x, -h * 0.5), p(x, h * 0.5)], stroke * 0.85, col);
            }
        }
        Icon::Plugin => {
            c.stroke_polyline(&[p(-2.5, -7.0), p(-2.5, -3.5)], stroke, col);
            c.stroke_polyline(&[p(2.5, -7.0), p(2.5, -3.5)], stroke, col);
            c.fill_rrect(rr(-5.5, -3.5, 11.0, 6.0), 2.0 * s, col);
            c.stroke_polyline(&[p(0.0, 2.5), p(0.0, 7.0)], stroke, col);
        }
        Icon::Mute => {
            let mut path = Path::new();
            path.polygon(&[p(-7.0, -2.5), p(-4.0, -2.5), p(0.0, -6.0), p(0.0, 6.0), p(-4.0, 2.5), p(-7.0, 2.5)]);
            c.fill_path(&path, col);
            c.stroke_polyline(&[p(2.5, -2.5), p(7.0, 2.5)], stroke, col);
            c.stroke_polyline(&[p(7.0, -2.5), p(2.5, 2.5)], stroke, col);
        }
        Icon::Solo => {
            c.stroke_arc(cx, cy + 1.0 * s, 6.0 * s, PI, 2.0 * PI, stroke, col);
            c.fill_rrect(rr(-7.2, 0.5, 3.6, 6.0), 1.2 * s, col);
            c.fill_rrect(rr(3.6, 0.5, 3.6, 6.0), 1.2 * s, col);
        }
        Icon::Record => {
            c.stroke_circle(cx, cy, 6.5 * s, stroke, col);
            c.fill_circle(cx, cy, 3.8 * s, col);
        }
        Icon::Pencil => {
            c.stroke_polyline(&[p(-6.5, 6.5), p(-6.5, 3.0), p(3.0, -6.5), p(6.5, -3.0), p(-3.0, 6.5), p(-6.5, 6.5)], stroke, col);
            c.stroke_polyline(&[p(0.5, -4.0), p(4.0, -0.5)], stroke, col);
        }
        Icon::Duplicate => {
            c.stroke_polyline(&[p(2.5, -4.0), p(2.5, -6.5), p(-6.5, -6.5), p(-6.5, 2.5), p(-4.0, 2.5)], stroke, col);
            c.stroke_rrect(rr(-2.5, -2.5, 9.0, 9.0), 1.8 * s, stroke, col);
        }
        Icon::Trash => {
            c.stroke_polyline(&[p(-7.0, -4.0), p(7.0, -4.0)], stroke, col);
            c.stroke_polyline(&[p(-2.0, -4.0), p(-2.0, -6.5), p(2.0, -6.5), p(2.0, -4.0)], stroke, col);
            c.stroke_polyline(&[p(-5.0, -4.0), p(-4.0, 6.5), p(4.0, 6.5), p(5.0, -4.0)], stroke, col);
        }
        Icon::Swap => {
            c.stroke_polyline(&[p(-6.5, -3.0), p(6.0, -3.0)], stroke, col);
            c.stroke_polyline(&[p(3.0, -6.0), p(6.5, -3.0), p(3.0, 0.0)], stroke, col);
            c.stroke_polyline(&[p(6.5, 3.5), p(-6.0, 3.5)], stroke, col);
            c.stroke_polyline(&[p(-3.0, 0.5), p(-6.5, 3.5), p(-3.0, 6.5)], stroke, col);
        }
        Icon::Route => {
            c.stroke_polyline(&[p(-7.0, 0.0), p(4.5, 0.0)], stroke, col);
            c.stroke_polyline(&[p(0.5, -4.0), p(4.5, 0.0), p(0.5, 4.0)], stroke, col);
            c.stroke_polyline(&[p(7.0, -5.5), p(7.0, 5.5)], stroke, col);
        }
        Icon::Send => {
            c.stroke_polyline(&[p(-6.0, 6.5), p(-6.0, -1.0), p(5.5, -1.0)], stroke, col);
            c.stroke_polyline(&[p(2.0, -4.5), p(5.5, -1.0), p(2.0, 2.5)], stroke, col);
        }
        Icon::Reset => {
            c.stroke_arc(cx, cy + 0.5 * s, 6.0 * s, -0.35 * PI, 1.35 * PI, stroke, col);
            c.stroke_polyline(&[p(1.0, -7.5), p(3.8, -4.8), p(0.8, -2.5)], stroke, col);
        }
        Icon::Automation => {
            let pts = [p(-7.0, 4.5), p(-2.5, -3.0), p(2.5, 2.0), p(7.0, -5.0)];
            c.stroke_polyline(&pts, stroke, col);
            for &(x, y) in &pts[1..3] {
                c.fill_circle(x, y, 2.2 * s, col);
            }
        }
        Icon::Preset => {
            c.stroke_polyline(&[p(-7.0, -3.5), p(7.0, -3.5)], stroke, col.alpha(0.6));
            c.stroke_polyline(&[p(-7.0, 3.5), p(7.0, 3.5)], stroke, col.alpha(0.6));
            c.fill_circle(cx + 2.5 * s, cy - 3.5 * s, 2.6 * s, col);
            c.fill_circle(cx - 3.0 * s, cy + 3.5 * s, 2.6 * s, col);
        }
        Icon::Info => {
            c.stroke_circle(cx, cy, 7.0 * s, stroke, col);
            c.fill_circle(cx, cy - 3.6 * s, 1.2 * s.max(0.9), col);
            c.stroke_polyline(&[p(0.0, -0.8), p(0.0, 4.2)], stroke, col);
        }
        Icon::Magnet => {
            c.stroke_arc(cx, cy + 0.5 * s, 4.5 * s, 0.0, PI, stroke * 1.5, col);
            c.stroke_polyline(&[p(-4.5, 0.5), p(-4.5, -3.0)], stroke * 1.5, col);
            c.stroke_polyline(&[p(4.5, 0.5), p(4.5, -3.0)], stroke * 1.5, col);
            c.fill_rect(rr(-5.7, -6.5, 2.4, 2.6), col.alpha(0.55));
            c.fill_rect(rr(3.3, -6.5, 2.4, 2.6), col.alpha(0.55));
        }
        Icon::Columns => {
            for i in 0..3 {
                c.stroke_rrect(rr(-7.0 + i as f32 * 5.0, -6.5, 4.0, 13.0), 1.2 * s, stroke * 0.85, col);
            }
        }
        Icon::Scale => {
            // Quatre marches posées sur la même ligne, de plus en plus hautes.
            for i in 0..4 {
                let h = 4.0 + i as f32 * 3.3;
                c.fill_rrect(rr(-7.0 + i as f32 * 3.8, 7.0 - h, 2.8, h), 1.0 * s, col.alpha(0.55 + 0.15 * i as f32));
            }
        }
        Icon::Chord => {
            for y in [5.2, -0.2, -5.6] {
                c.fill_circle(cx - 3.2 * s, cy + y * s, 2.0 * s, col);
            }
            c.stroke_polyline(&[p(-0.7, 5.2), p(-0.7, -7.5), p(4.5, -5.0)], stroke, col);
        }
        Icon::Dice => {
            c.stroke_rrect(rr(-6.5, -6.5, 13.0, 13.0), 3.0 * s, stroke, col);
            for (x, y) in [(-3.0, -3.0), (0.0, 0.0), (3.0, 3.0)] {
                c.fill_circle(cx + x * s, cy + y * s, 1.4 * s.max(0.9), col);
            }
        }
        Icon::Arpeggio => {
            for i in 0..4 {
                let (x, y) = (-7.0 + i as f32 * 3.8, 3.5 - i as f32 * 3.4);
                c.fill_rrect(rr(x, y, 3.2, 3.0), 1.0 * s, col.alpha(0.55 + 0.15 * i as f32));
            }
        }
        Icon::Repeat => {
            for i in 0..4 {
                let h = 13.0 - i as f32 * 3.0;
                c.fill_rrect(rr(-7.0 + i as f32 * 3.8, 6.5 - h, 2.6, h), 1.0 * s, col.alpha(1.0 - 0.18 * i as f32));
            }
        }
        Icon::History => {
            c.stroke_circle(cx, cy, 6.8 * s, stroke, col);
            c.stroke_polyline(&[p(0.0, -4.0), p(0.0, 0.0), p(3.0, 2.0)], stroke, col);
        }
        Icon::Menu => {
            for y in [-4.5, 0.0, 4.5] {
                c.stroke_polyline(&[p(-6.0, y), p(6.0, y)], stroke, col);
            }
        }
        Icon::Theme => {
            c.stroke_circle(cx, cy, 6.5 * s, stroke, col);
            let mut half = Path::new();
            half.polygon(&[p(0.0, -6.5), p(4.6, -4.6), p(6.5, 0.0), p(4.6, 4.6), p(0.0, 6.5)]);
            c.fill_path(&half, col);
        }
        Icon::Mic => {
            c.stroke_rrect(rr(-2.8, -7.5, 5.6, 10.0), 2.8 * s, stroke, col);
            c.stroke_arc(cx, cy - 1.0 * s, 5.2 * s, 0.15, std::f32::consts::PI - 0.15, stroke, col);
            c.stroke_polyline(&[p(0.0, 4.2), p(0.0, 7.5)], stroke, col);
            c.stroke_polyline(&[p(-3.0, 7.5), p(3.0, 7.5)], stroke, col);
        }
        Icon::Key => {
            c.stroke_circle(cx - 3.5 * s, cy, 3.6 * s, stroke, col);
            c.stroke_polyline(&[p(0.1, 0.0), p(7.5, 0.0)], stroke, col);
            c.stroke_polyline(&[p(4.2, 0.0), p(4.2, 3.2)], stroke, col);
            c.stroke_polyline(&[p(6.8, 0.0), p(6.8, 2.6)], stroke, col);
        }
        Icon::Detach => {
            c.stroke_polyline(&[p(-1.0, -6.5), p(-7.0, -6.5), p(-7.0, 6.5), p(6.5, 6.5), p(6.5, 1.0)], stroke, col);
            c.stroke_polyline(&[p(-1.5, 1.5), p(6.5, -6.5)], stroke, col);
            c.stroke_polyline(&[p(1.5, -6.5), p(6.5, -6.5), p(6.5, -1.5)], stroke, col);
        }
        Icon::Bus => {
            for y in [-5.5, 0.0, 5.5] {
                c.stroke_polyline(&[p(-7.0, y), p(-4.5, y), p(0.5, 0.0)], stroke, col);
            }
            c.stroke_polyline(&[p(0.5, 0.0), p(7.0, 0.0)], stroke, col);
            c.fill_circle(cx + 0.5 * s, cy, 2.1 * s, col);
        }
        Icon::Vca => {
            for x in [-4.0, 4.0] {
                c.stroke_polyline(&[p(x, -7.0), p(x, 7.0)], stroke * 0.8, col.alpha(0.55));
            }
            c.stroke_polyline(&[p(-4.0, -1.0), p(4.0, -1.0)], stroke, col);
            for x in [-4.0, 4.0] {
                c.fill_rrect(rr(x - 2.8, -3.0, 5.6, 4.0), 1.2 * s, col);
            }
        }
        Icon::Dock => {
            c.stroke_rrect(rr(-7.0, -6.5, 14.0, 13.0), 2.0 * s, stroke, col);
            c.fill_rect(rr(-7.0, 2.5, 14.0, 4.0), col.alpha(0.55));
            c.stroke_polyline(&[p(0.0, -4.5), p(0.0, 0.5)], stroke, col);
            c.stroke_polyline(&[p(-3.0, -2.0), p(0.0, 1.0), p(3.0, -2.0)], stroke, col);
        }
        Icon::Lab => {
            // Forme d'onde coupée par un marqueur à fanion.
            for (i, h) in [3.0, 8.0, 12.0, 6.0, 10.0, 4.0].into_iter().enumerate() {
                let x = -7.0 + i as f32 * 2.6 + if i >= 3 { 1.6 } else { 0.0 };
                c.stroke_polyline(&[p(x, 1.0 - h * 0.5), p(x, 1.0 + h * 0.5)], stroke * 0.8, if i >= 3 { col.alpha(0.55) } else { col });
            }
            c.stroke_polyline(&[p(0.6, -7.0), p(0.6, 7.5)], stroke * 0.8, col);
            let mut path = Path::new();
            path.polygon(&[p(0.6, -7.5), p(5.0, -5.5), p(0.6, -3.5)]);
            c.fill_path(&path, col);
        }
        Icon::Slice => {
            // Trois tranches séparées par deux marqueurs.
            c.fill_rrect(rr(-7.5, -3.0, 4.0, 9.0), 1.0 * s, col.alpha(0.55));
            c.fill_rrect(rr(-2.0, -1.0, 4.0, 7.0), 1.0 * s, col);
            c.fill_rrect(rr(3.5, 1.0, 4.0, 5.0), 1.0 * s, col.alpha(0.55));
            for x in [-2.75, 2.75] {
                let mut path = Path::new();
                path.polygon(&[p(x - 2.2, -7.0), p(x + 2.2, -7.0), p(x, -4.2)]);
                c.fill_path(&path, col);
            }
        }
        Icon::Globe => {
            // Sphère, méridien en ellipse (deux polylignes) et deux parallèles.
            c.stroke_circle(cx, cy, 7.0 * s, stroke, col);
            let meridian: Vec<_> = (0..=16).map(|i| {
                let a = -std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::PI / 16.0;
                (a.cos() * 3.2, a.sin() * 7.0)
            }).collect();
            c.stroke_polyline(&meridian.iter().map(|&(x, y)| p(x, y)).collect::<Vec<_>>(), stroke * 0.8, col);
            c.stroke_polyline(&meridian.iter().map(|&(x, y)| p(-x, y)).collect::<Vec<_>>(), stroke * 0.8, col);
            c.stroke_polyline(&[p(-7.0, 0.0), p(7.0, 0.0)], stroke * 0.8, col);
            c.stroke_polyline(&[p(-5.6, -3.6), p(5.6, -3.6)], stroke * 0.6, col.alpha(0.6));
            c.stroke_polyline(&[p(-5.6, 3.6), p(5.6, 3.6)], stroke * 0.6, col.alpha(0.6));
        }
        Icon::PluginWindow => {
            // Frame and title bar, a knob turned up, a fader.
            c.stroke_rrect(rr(-7.5, -6.5, 15.0, 13.0), 2.2 * s, stroke, col);
            c.fill_rrect(rr(-7.5, -6.5, 15.0, 3.4), 1.6 * s, col);
            c.stroke_circle(cx - 2.6 * s, cy + 1.9 * s, 2.5 * s, stroke * 0.9, col);
            c.stroke_polyline(&[p(-2.6, 1.9), p(-1.1, 0.3)], stroke * 0.9, col);
            c.stroke_polyline(&[p(3.6, -0.6), p(3.6, 4.6)], stroke * 0.7, col.alpha(0.5));
            c.fill_rrect(rr(2.1, 1.2, 3.0, 1.8), 0.8 * s, col);
        }
        Icon::Stretch => {
            c.stroke_polyline(&[p(-7.0, 0.0), p(7.0, 0.0)], stroke, col);
            c.stroke_polyline(&[p(-4.0, -3.0), p(-7.0, 0.0), p(-4.0, 3.0)], stroke, col);
            c.stroke_polyline(&[p(4.0, -3.0), p(7.0, 0.0), p(4.0, 3.0)], stroke, col);
            c.stroke_polyline(&[p(-1.5, -6.5), p(-1.5, -3.5)], stroke * 0.8, col.alpha(0.6));
            c.stroke_polyline(&[p(1.5, 3.5), p(1.5, 6.5)], stroke * 0.8, col.alpha(0.6));
        }
        Icon::Normalize => {
            c.stroke_polyline(&[p(-7.0, -6.0), p(7.0, -6.0)], stroke * 0.8, col);
            for (i, h) in [5.0, 9.0, 11.0, 7.0].into_iter().enumerate() {
                let x = -4.5 + i as f32 * 3.0;
                c.fill_rrect(rr(x - 1.0, 7.0 - h, 2.0, h), 1.0 * s, if h > 10.0 { col } else { col.alpha(0.6) });
            }
        }
        Icon::Spectral => {
            // Barres en dégradé : grave sombre, aigu clair.
            for (i, h) in [11.0, 8.0, 10.0, 6.0, 4.0].into_iter().enumerate() {
                let x = -7.0 + i as f32 * 3.4;
                c.fill_rrect(rr(x, 5.5 - h, 2.4, h), 1.0 * s, col.alpha(0.35 + 0.16 * i as f32));
            }
        }
        Icon::Check => c.stroke_polyline(&[p(-6.0, 0.5), p(-2.0, 4.5), p(6.0, -4.5)], stroke * 1.15, col),
        Icon::Play => {
            let mut path = Path::new();
            path.polygon(&[p(-4.0, -6.0), p(6.5, 0.0), p(-4.0, 6.0)]);
            c.fill_path(&path, col);
        }
        Icon::Pause => {
            c.fill_rrect(Rect::new(cx - 5.0 * s, cy - 6.0 * s, 3.5 * s, 12.0 * s), 1.0, col);
            c.fill_rrect(Rect::new(cx + 1.5 * s, cy - 6.0 * s, 3.5 * s, 12.0 * s), 1.0, col);
        }
        Icon::Stop => c.fill_rrect(Rect::new(cx - 5.5 * s, cy - 5.5 * s, 11.0 * s, 11.0 * s), 2.0 * s, col),
        Icon::Plus => {
            c.fill_rrect(Rect::new(cx - 6.0 * s, cy - 0.8 * s, 12.0 * s, 1.6 * s), 0.8, col);
            c.fill_rrect(Rect::new(cx - 0.8 * s, cy - 6.0 * s, 1.6 * s, 12.0 * s), 0.8, col);
        }
        Icon::Close => {
            c.stroke_polyline(&[p(-5.0, -5.0), p(5.0, 5.0)], stroke, col);
            c.stroke_polyline(&[p(5.0, -5.0), p(-5.0, 5.0)], stroke, col);
        }
        Icon::Metronome => {
            c.stroke_polyline(&[p(-5.5, 6.5), p(-2.5, -6.5), p(2.5, -6.5), p(5.5, 6.5), p(-5.5, 6.5)], stroke, col);
            c.stroke_polyline(&[p(0.0, 3.0), p(4.5, -4.0)], stroke, col);
        }
        Icon::Undo | Icon::Redo => {
            // Demi-cercle ouvert d'un côté, flèche en haut qui repart vers l'arrière.
            let m = if icon == Icon::Undo { 1.0 } else { -1.0 };
            let pts: Vec<(f32, f32)> = (0..=12)
                .map(|i| {
                    let a = -PI * 0.5 + PI * i as f32 / 12.0;
                    p(m * (a.cos() * 5.0 - 1.0), a.sin() * 5.0 + 1.0)
                })
                .collect();
            c.stroke_polyline(&pts, stroke, col);
            c.stroke_polyline(&[p(-4.0 * m, -1.0), p(-m, -4.0), p(m * 2.0, -4.0)], stroke, col);
            c.stroke_polyline(&[p(-4.0 * m, -1.0), p(-m, -4.0)], stroke, col);
            c.stroke_polyline(&[p(-4.0 * m, -7.0), p(-m, -4.0)], stroke, col);
        }
        Icon::Save => {
            c.stroke_rrect(Rect::new(cx - 6.0 * s, cy - 6.0 * s, 12.0 * s, 12.0 * s), 2.0 * s, stroke, col);
            c.fill_rect(Rect::new(cx - 3.0 * s, cy - 6.0 * s, 6.0 * s, 4.0 * s), col);
        }
        Icon::Folder => {
            c.stroke_polyline(
                &[p(-7.0, -5.0), p(-2.0, -5.0), p(0.0, -3.0), p(7.0, -3.0), p(7.0, 6.0), p(-7.0, 6.0), p(-7.0, -5.0)],
                stroke,
                col,
            );
        }
        Icon::Export => {
            c.stroke_polyline(&[p(0.0, 3.0), p(0.0, -7.0)], stroke, col);
            c.stroke_polyline(&[p(-4.0, -3.0), p(0.0, -7.0), p(4.0, -3.0)], stroke, col);
            c.stroke_polyline(&[p(-6.0, 1.0), p(-6.0, 6.5), p(6.0, 6.5), p(6.0, 1.0)], stroke, col);
        }
        Icon::Sidebar | Icon::Panel => {
            c.stroke_rrect(Rect::new(cx - 7.0 * s, cy - 6.0 * s, 14.0 * s, 12.0 * s), 2.0 * s, stroke, col);
            if icon == Icon::Sidebar {
                c.fill_rect(Rect::new(cx - 2.5 * s, cy - 6.0 * s, stroke, 12.0 * s), col);
            } else {
                c.fill_rect(Rect::new(cx - 7.0 * s, cy + 1.5 * s, 14.0 * s, stroke), col);
            }
        }
        Icon::Layout => {
            c.stroke_rrect(rr(-7.5, -6.5, 15.0, 13.0), 2.0 * s, stroke, col);
            c.fill_rect(Rect::new(cx - 7.5 * s, cy - 3.0 * s, 15.0 * s, stroke), col);
            c.fill_rect(Rect::new(cx - 2.5 * s, cy - 3.0 * s, stroke, 9.5 * s), col);
            c.fill_rrect(rr(0.5, -0.2, 4.5, 3.6), 1.0 * s, col.alpha(0.6));
        }
        Icon::ChevronLeft => c.stroke_polyline(&[p(2.5, -5.0), p(-2.5, 0.0), p(2.5, 5.0)], stroke, col),
        Icon::ChevronRight => c.stroke_polyline(&[p(-2.5, -5.0), p(2.5, 0.0), p(-2.5, 5.0)], stroke, col),
        Icon::ChevronDown => c.stroke_polyline(&[p(-5.0, -2.5), p(0.0, 2.5), p(5.0, -2.5)], stroke, col),
        Icon::Search => {
            c.stroke_circle(cx - 1.5 * s, cy - 1.5 * s, 5.0 * s, stroke, col);
            c.stroke_polyline(&[p(2.5, 2.5), p(6.5, 6.5)], stroke, col);
        }
        Icon::Keyboard => {
            c.stroke_rrect(Rect::new(cx - 7.5 * s, cy - 5.5 * s, 15.0 * s, 11.0 * s), 2.0 * s, stroke, col);
            for i in 0..3 {
                c.fill_rect(Rect::new(cx + (-3.5 + i as f32 * 3.5) * s - 0.75, cy - 5.5 * s, 1.5, 6.0 * s), col);
            }
        }
        Icon::Power => {
            c.stroke_arc(cx, cy + 0.5 * s, 5.5 * s, -PI * 0.5 + 0.7, PI * 1.5 - 0.7, stroke, col);
            c.stroke_polyline(&[p(0.0, -7.0), p(0.0, -1.0)], stroke, col);
        }
    }
}

impl App {
    // --- Texte ---

    pub(super) fn text(&mut self, c: &mut Canvas, f: FontId, size: f32, x: f32, cy: f32, s: &str, col: Color) -> f32 {
        let baseline = cy + self.fonts.cap_height(f, size) * 0.5;
        self.fonts.draw(c, f, size, x, baseline, s, col)
    }

    pub(super) fn text_right(&mut self, c: &mut Canvas, f: FontId, size: f32, right: f32, cy: f32, s: &str, col: Color) -> f32 {
        let w = self.fonts.measure(f, size, s);
        self.text(c, f, size, right - w, cy, s, col);
        right - w
    }

    pub(super) fn text_center(&mut self, c: &mut Canvas, f: FontId, size: f32, cx: f32, cy: f32, s: &str, col: Color) {
        let w = self.fonts.measure(f, size, s);
        self.text(c, f, size, cx - w * 0.5, cy, s, col);
    }

    /// Texte tronqué avec « … » pour tenir dans `max_w`.
    /// Shrinks the text by up to 2 px before cutting it: the largest size from
    /// `size` down to `size - 2` where `s` fits, otherwise `s` cut at the smallest.
    pub(super) fn fit_shrink(&self, f: FontId, size: f32, s: &str, max_w: f32) -> (f32, String) {
        for k in 0..=2 {
            let sz = size - k as f32;
            if self.fonts.measure(f, sz, s) <= max_w {
                return (sz, s.to_string());
            }
        }
        (size - 2.0, self.fit(f, size - 2.0, s, max_w))
    }

    pub(super) fn fit(&self, f: FontId, size: f32, s: &str, max_w: f32) -> String {
        self.fit_cow(f, size, s, max_w).into_owned()
    }

    /// `s` s'il tient dans `max_w`, sinon son début suivi de « … ». Sans
    /// allocation quand le texte tient ; sinon la coupe est cherchée par
    /// dichotomie (quelques mesures, plus une par caractère retiré).
    pub(super) fn fit_cow<'a>(&self, f: FontId, size: f32, s: &'a str, max_w: f32) -> std::borrow::Cow<'a, str> {
        use std::borrow::Cow;
        if self.fonts.measure(f, size, s) <= max_w {
            return Cow::Borrowed(s);
        }
        let ellipsis = self.fonts.measure(f, size, "…");
        let prefix = |k: usize| s[..s.char_indices().nth(k).map_or(s.len(), |(i, _)| i)].trim_end();
        let fits = |k: usize| self.fonts.measure(f, size, prefix(k)) + ellipsis <= max_w;
        // Plus long début qui tient (la largeur croît avec le nombre de caractères).
        let (mut lo, mut hi) = (0, s.chars().count());
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            if fits(mid) {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        if lo == 0 {
            return Cow::Borrowed("…");
        }
        Cow::Owned(format!("{}…", prefix(lo)))
    }

    pub(super) fn text_fit(&mut self, c: &mut Canvas, f: FontId, size: f32, x: f32, cy: f32, s: &str, max_w: f32, col: Color) {
        let s = self.fit_cow(f, size, s, max_w);
        self.text(c, f, size, x, cy, &s, col);
    }

    // --- Interaction ---

    /// Survol : la souris est dessus et aucun autre widget n'est saisi.
    pub(super) fn hovered(&self, input: &Input, id: u64, r: Rect) -> bool {
        input.inside && r.contains(input.x, input.y) && self.drag.as_ref().is_none_or(|d| d.id == id)
    }

    pub(super) fn is_dragging(&self, id: u64) -> bool {
        self.drag.as_ref().is_some_and(|d| d.id == id)
    }

    pub(super) fn begin_drag(&mut self, id: u64, input: &Input, v0: f32) {
        self.drag = Some(Drag { id, y0: input.y, v0, fine: input.mods.shift });
    }

    /// Forme du curseur pour cette image (le dernier qui la demande gagne).
    pub(super) fn set_cursor(&mut self, cursor: Cursor) {
        self.cursor = cursor;
    }

    /// Valeur 0..1 animée de survol pour `id`.
    pub(super) fn hover_anim(&mut self, id: u64, on: bool) -> f32 {
        self.anim.to(id ^ 0x5157_4552, on as u8 as f32, T_HOVER)
    }

    /// Bouton icône carré ; `on` le montre enfoncé (bascule active).
    /// `tip` : libellé et raccourci de l'infobulle.
    pub(super) fn icon_button(&mut self, c: &mut Canvas, input: &Input, id: u64, r: Rect, icon: Icon, on: bool, tip: Tip) -> bool {
        let t = self.theme;
        let hov = self.hovered(input, id, r);
        let hv = self.hover_anim(id, hov);
        let onv = self.anim.to(id ^ 0x4f4e, on as u8 as f32, T_HOVER);
        let press = self.anim.to(id ^ 0x5052, if hov && input.down { 0.92 } else { 1.0 }, T_PRESS);
        let fill = t.raised.alpha(hv.max(onv)).mix(t.raised_hi, onv * 0.5);
        c.fill_rrect(r.scale(press), 8.0, fill);
        let col = t.text2.mix(t.text, hv).mix(t.accent, onv);
        let (cx, cy) = r.center();
        let size = if r.h >= 30.0 { 17.0 } else { 15.0 };
        draw_icon(c, icon, cx, cy, size * press, col);
        if hov {
            self.set_tip(tip.0, tip.1);
            self.set_cursor(Cursor::Hand);
        }
        hov && input.pressed
    }

    /// Sélecteur à segments de texte court, chacun avec son infobulle.
    pub(super) fn segmented_tips(&mut self, c: &mut Canvas, input: &Input, id: u64, r: Rect, opts: &[&str], tips: &[Tip], sel: usize) -> Option<usize> {
        let segs: Vec<Seg> = opts.iter().map(|&s| Seg::Text(s)).collect();
        self.segmented_with(c, input, id, r, &segs, tips, sel)
    }

    /// Sélecteur à segments d'icônes, chacune avec son infobulle.
    pub(super) fn icon_segmented(&mut self, c: &mut Canvas, input: &Input, id: u64, r: Rect, icons: &[Icon], tips: &[Tip], sel: usize) -> Option<usize> {
        let segs: Vec<Seg> = icons.iter().map(|&i| Seg::Icon(i)).collect();
        self.segmented_with(c, input, id, r, &segs, tips, sel)
    }

    fn segmented_with(&mut self, c: &mut Canvas, input: &Input, id: u64, r: Rect, opts: &[Seg], tips: &[Tip], sel: usize) -> Option<usize> {
        let t = self.theme;
        c.fill_rrect(r, 10.0, t.bg_deep);
        c.stroke_rrect(r, 10.0, 1.0, t.hairline);
        let n = opts.len().max(1) as f32;
        let seg_w = (r.w - 6.0) / n;
        let x = self.anim.to(id, sel as f32, T_LAYOUT);
        let knob = Rect::new(r.x + 3.0 + x * seg_w, r.y + 3.0, seg_w, r.h - 6.0);
        c.fill_rrect(knob, 7.0, t.raised_hi);
        c.fill_rect(Rect::new(knob.x + 6.0, knob.y + 1.0, knob.w - 12.0, 1.0), WHITE.alpha(0.06));
        let mut picked = None;
        for (i, opt) in opts.iter().enumerate() {
            let seg = Rect::new(r.x + 3.0 + i as f32 * seg_w, r.y, seg_w, r.h);
            let hov = self.hovered(input, id ^ (i as u64 + 1), seg);
            if hov && input.pressed && i != sel {
                picked = Some(i);
            }
            if hov && i != sel {
                self.set_cursor(Cursor::Hand);
            }
            if hov && let Some(tip) = tips.get(i) {
                self.set_tip(tip.0, tip.1);
            }
            let hv = self.hover_anim(id ^ (i as u64 + 1), hov);
            let selected = 1.0 - (x - i as f32).abs().min(1.0);
            let (cx, cy) = seg.center();
            match *opt {
                Seg::Text(s) => {
                    let col = t.muted.mix(t.text2, hv).mix(t.text, selected);
                    let (size, s) = self.fit_shrink(self.f.sans_med, 13.0, s, seg_w - 10.0);
                    self.text_center(c, self.f.sans_med, size, cx, cy, &s, col);
                }
                Seg::Icon(icon) => {
                    let col = t.muted.mix(t.text, hv * 0.7).mix(t.accent, selected);
                    draw_icon(c, icon, cx, cy, 17.0, col);
                }
            }
        }
        picked
    }

    // --- Infobulles ---

    /// Infobulle du contrôle survolé (le dernier qui la demande gagne).
    pub(super) fn set_tip(&mut self, label: &str, key: &str) {
        if !label.is_empty() && self.drag.is_none() {
            self.tips.want = Some((label.to_string(), key.to_string()));
        }
    }

    /// Infobulle d'une zone, si la souris est dessus.
    pub(super) fn tip_zone(&mut self, input: &Input, r: Rect, label: &str, key: &str) {
        if input.inside && r.contains(input.x, input.y) {
            self.set_tip(label, key);
        }
    }

    /// Touche de raccourci dessinée comme une touche de clavier, alignée à droite
    /// sur `right` ; renvoie son bord gauche.
    pub(super) fn keycap(&mut self, c: &mut Canvas, right: f32, cy: f32, key: &str, alpha: f32) -> f32 {
        let t = self.theme;
        let kw = self.fonts.measure(self.f.mono, 11.0, key) + 12.0;
        let cap = Rect::new(right - kw, cy - 10.0, kw, 20.0);
        c.fill_rrect(cap, 5.0, t.bg_deep.mix(t.raised_hi, 0.5).alpha(alpha));
        c.stroke_rrect(cap, 5.0, 1.0, t.hairline.alpha(alpha * 1.5));
        self.text_center(c, self.f.mono, 11.0, cap.x + cap.w * 0.5, cy, key, t.text2.alpha(alpha));
        cap.x
    }

    /// Dessine l'infobulle après ~450 ms de survol immobile ; fondu d'entrée.
    pub(super) fn tooltip_layer(&mut self, c: &mut Canvas, input: &Input, screen: Rect) {
        let t = self.theme;
        let want = self.tips.want.take();
        let Some(want) = want else {
            self.tips.current = None;
            self.tips.waiting = false;
            return;
        };
        if self.tips.current.as_ref() != Some(&want) {
            // Une infobulle vient d'être vue : la suivante s'ouvre presque tout de suite.
            let warm = self.time - self.tips.last_shown < 0.3;
            self.tips.since = self.time - if warm { 0.4 } else { 0.0 };
            self.tips.current = Some(want.clone());
            self.tips.blocked = false;
            self.anim.set(wid("tip", 0, 0), 0.0);
        }
        if input.pressed || input.right_pressed || input.wheel != 0.0 || input.down {
            self.tips.blocked = true;
        }
        let ready = !self.tips.blocked && self.time - self.tips.since >= 0.45;
        self.tips.waiting = !self.tips.blocked && !ready;
        if !ready {
            return;
        }
        self.tips.last_shown = self.time;
        let a = self.anim.to(wid("tip", 0, 0), 1.0, T_HOVER);
        let (label, key) = want;
        let lines: Vec<&str> = label.lines().collect();
        let mut w: f32 = 0.0;
        for (i, l) in lines.iter().enumerate() {
            let f = if i == 0 { self.f.sans_med } else { self.f.sans };
            w = w.max(self.fonts.measure(f, 12.0, l));
        }
        let key_w = if key.is_empty() { 0.0 } else { self.fonts.measure(self.f.mono, 11.0, &key) + 12.0 + 10.0 };
        let first_w = self.fonts.measure(self.f.sans_med, 12.0, lines.first().copied().unwrap_or("")) + key_w;
        w = w.max(first_w) + 20.0;
        let h = 12.0 + lines.len() as f32 * 18.0 + if lines.len() > 1 { 2.0 } else { 0.0 };
        let mut x = input.x + 12.0;
        let mut y = input.y + 22.0;
        if x + w > screen.right() - 8.0 {
            x = (screen.right() - 8.0 - w).max(8.0);
        }
        if y + h > screen.bottom() - 8.0 {
            y = input.y - h - 10.0;
        }
        let r = Rect::new(x.round(), (y + (1.0 - a) * 4.0).round(), w, h);
        c.fill_rrect(r.translate(0.0, 4.0).inset(-1.0), 10.0, Color::rgba(0, 0, 0, (70.0 * a) as u8));
        c.fill_rrect(r, 8.0, t.raised_hi.mix(t.bg_deep, 0.15).alpha(a));
        c.stroke_rrect(r, 8.0, 1.0, t.hairline.alpha(a * 1.6));
        for (i, l) in lines.iter().enumerate() {
            let cy = r.y + 15.0 + i as f32 * 18.0 + if i > 0 { 2.0 } else { 0.0 };
            if i == 0 {
                self.text(c, self.f.sans_med, 12.0, r.x + 10.0, cy, l, t.text.alpha(a));
                if !key.is_empty() {
                    self.keycap(c, r.right() - 6.0, cy, &key, a);
                }
            } else {
                self.text(c, self.f.sans, 12.0, r.x + 10.0, cy, l, t.text2.alpha(a));
            }
        }
    }

    /// Libellé raccourci intelligemment pour tenir dans `max_w` : « Hauteur de
    /// départ » → « H. départ », « Compensation » → « Comp. ».
    pub(super) fn short_label(&self, f: FontId, size: f32, s: &str, max_w: f32) -> String {
        let fits = |x: &str| self.fonts.measure(f, size, x) <= max_w;
        if fits(s) {
            return s.to_string();
        }
        let words: Vec<&str> = s.split_whitespace().filter(|w| !matches!(*w, "de" | "du" | "des" | "la" | "le" | "les" | "à" | "d'" | "of" | "the")).collect();
        if words.len() > 1 {
            let last = words[words.len() - 1];
            let initials: String = words[..words.len() - 1].iter().filter_map(|w| w.chars().next()).map(|ch| format!("{ch}. ")).collect();
            let cand = format!("{initials}{last}");
            if fits(&cand) {
                return cand;
            }
            return self.short_label(f, size, last, max_w);
        }
        // Un seul mot : le plus long préfixe qui tient, coupé après une consonne.
        let chars: Vec<char> = s.chars().collect();
        for n in (2..chars.len()).rev() {
            if "aeiouyéèêàâîôûAEIOUY".contains(chars[n - 1]) {
                continue;
            }
            let cand: String = chars[..n].iter().collect::<String>() + ".";
            if fits(&cand) {
                return cand;
            }
        }
        self.fit(f, size, s, max_w)
    }

    /// Potard de paramètre dans une cellule : nom, potard, valeur.
    /// Glisser verticalement (Maj : fin), molette, double-clic : valeur par défaut.
    pub(super) fn knob(&mut self, c: &mut Canvas, input: &Input, id: u64, cell: Rect, info: &ParamInfo, value: f32) -> Option<f32> {
        let t = self.theme;
        let (cx, cy) = (cell.x + cell.w * 0.5, cell.y + 34.0);
        let hit = Rect::new(cell.x, cell.y + 12.0, cell.w, 46.0);
        let hov = self.hovered(input, id, hit);
        let mut changed = None;
        let norm = info.to_norm(value);
        if hov && input.pressed {
            if input.double || input.mods.ctrl {
                changed = Some(info.default);
            } else {
                self.begin_drag(id, input, norm);
            }
        }
        if let Some(d) = self.drag.as_ref().filter(|d| d.id == id) {
            let per_px = if d.fine { 0.0012 } else { 0.006 };
            let n = (d.v0 + (d.y0 - input.y) * per_px).clamp(0.0, 1.0);
            let v = info.from_norm(n);
            if v != value {
                changed = Some(v);
            }
        }
        if hov && input.wheel != 0.0 {
            let step = if info.unit == Unit::Choice { 1.0 / (info.max - info.min).max(1.0) } else { 0.02 };
            changed = Some(info.from_norm(norm + input.wheel * step));
        }
        let active = hov || self.is_dragging(id);
        if active {
            self.set_cursor(Cursor::ResizeV);
        }
        let hv = self.hover_anim(id, active);

        let name = self.short_label(self.f.sans, 12.0, ponzik_i18n::tr(info.name), cell.w - 4.0);
        self.text_center(c, self.f.sans, 12.0, cx, cell.y + 6.0, &name, t.text2.mix(t.text, hv));
        if hov {
            self.set_tip(&ponzik_i18n::tr_fmt!("{}\nDouble-click: default · right-click: automate", ponzik_i18n::tr(info.name)), "");
        }

        let (a0, sweep) = (0.75 * PI, 1.5 * PI);
        let shown = changed.map_or(norm, |v| info.to_norm(v));
        let a = a0 + sweep * shown;
        c.stroke_arc(cx, cy, 17.0, a0, a0 + sweep, 3.0, t.raised_hi);
        let bipolar = info.min < 0.0 && info.max > 0.0;
        let from = if bipolar { a0 + sweep * info.to_norm(0.0) } else { a0 };
        if (a - from).abs() > 0.01 {
            c.stroke_arc(cx, cy, 17.0, from.min(a), from.max(a), 3.0, t.accent.mix(t.accent_hot, hv * 0.25));
        }
        c.fill_circle(cx, cy, 11.0, t.raised_hi.mix(Color::hex(0x34403e), hv));
        c.stroke_circle(cx, cy, 11.0, 1.0, t.hairline);
        let (dx, dy) = (a.cos(), a.sin());
        c.stroke_polyline(&[(cx + dx * 3.5, cy + dy * 3.5), (cx + dx * 9.5, cy + dy * 9.5)], 2.0, t.text);

        let label = info.format(changed.unwrap_or(value));
        let label = self.fit(self.f.mono, 11.0, &label, cell.w - 2.0);
        self.text_center(c, self.f.mono, 11.0, cx, cell.y + 62.0, &label, t.muted.mix(t.text, hv));
        changed
    }

    /// Paramètre à choix : nom, puis une pastille avec flèches de part et d'autre.
    pub(super) fn choice(&mut self, c: &mut Canvas, input: &Input, id: u64, cell: Rect, info: &ParamInfo, value: f32) -> Option<f32> {
        let t = self.theme;
        let label = ponzik_i18n::tr(info.name);
        let name = self.short_label(self.f.sans, 12.0, label, cell.w - 4.0);
        self.text_center(c, self.f.sans, 12.0, cell.x + cell.w * 0.5, cell.y + 6.0, &name, t.text2);
        let field = Rect::new(cell.x + 2.0, cell.y + 20.0, cell.w - 4.0, 30.0);
        let hov = self.hovered(input, id, field);
        let hv = self.hover_anim(id, hov);
        c.fill_rrect(field, 8.0, t.raised.mix(t.raised_hi, hv));
        c.stroke_rrect(field, 8.0, 1.0, t.hairline);
        let count = (info.max - info.min).round() as i32 + 1;
        let cur = value.round() as i32;
        let mut changed = None;
        if hov && name != label {
            self.set_tip(label, "");
        }
        if hov {
            self.set_cursor(Cursor::Hand);
        }
        if hov && input.pressed {
            let dir = if input.x < field.x + field.w * 0.3 { -1 } else { 1 };
            changed = Some(((cur + dir).rem_euclid(count.max(1))) as f32);
        }
        if hov && input.wheel != 0.0 {
            changed = Some(((cur - input.wheel.signum() as i32).clamp(0, count - 1)) as f32);
        }
        let (fx, fy) = field.center();
        let label = info.format(changed.unwrap_or(value));
        // A long choice pulls the arrows to the edges before it gets cut.
        let (mut size, mut shown) = self.fit_shrink(self.f.sans_med, 12.0, &label, field.w - 34.0);
        let mut inset = 10.0;
        if shown != label {
            (size, shown) = self.fit_shrink(self.f.sans_med, 12.0, &label, field.w - 22.0);
            inset = 6.0;
        }
        draw_icon(c, Icon::ChevronLeft, field.x + inset, fy, 12.0 - (10.0 - inset) * 0.5, t.muted.mix(t.text2, hv));
        draw_icon(c, Icon::ChevronRight, field.right() - inset, fy, 12.0 - (10.0 - inset) * 0.5, t.muted.mix(t.text2, hv));
        self.text_center(c, self.f.sans_med, size, fx, fy, &shown, t.text);
        changed
    }

    /// Champ numérique réglé en glissant verticalement ou à la molette.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn drag_field(
        &mut self,
        c: &mut Canvas,
        input: &Input,
        id: u64,
        r: Rect,
        value: f32,
        per_px: f32,
        wheel_step: f32,
        text: &str,
        unit: &str,
    ) -> Option<f32> {
        let t = self.theme;
        let hov = self.hovered(input, id, r);
        if hov && input.pressed {
            self.begin_drag(id, input, value);
        }
        let mut changed = None;
        if let Some(d) = self.drag.as_ref().filter(|d| d.id == id) {
            let k = if d.fine { 0.1 } else { 1.0 };
            changed = Some(d.v0 + (d.y0 - input.y) * per_px * k);
        }
        if hov && input.wheel != 0.0 {
            changed = Some(value + input.wheel * wheel_step * if input.mods.shift { 0.1 } else { 1.0 });
        }
        if hov || self.is_dragging(id) {
            self.set_cursor(Cursor::ResizeV);
        }
        let hv = self.hover_anim(id, hov || self.is_dragging(id));
        c.fill_rrect(r, 10.0, t.raised.mix(t.raised_hi, hv));
        c.stroke_rrect(r, 10.0, 1.0, t.hairline);
        let (_, cy) = r.center();
        self.text(c, self.f.mono_med, 17.0, r.x + 12.0, cy, text, t.text);
        self.text_right(c, self.f.sans, 11.0, r.right() - 10.0, cy, unit, t.muted);
        changed
    }
}
