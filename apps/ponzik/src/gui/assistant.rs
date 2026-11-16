//! Assistant de mixage : écoute avec anneau de progression, spectre mesuré
//! contre la cible du profil, EQ en place et EQ suggérée, cartes des modules
//! avec leurs indicateurs, réglages du module choisi. Rien ne s'applique sans
//! un clic sur « Apply suggestion ».

use std::cell::RefCell;
use std::collections::HashMap;
use std::f32::consts::{PI, TAU};
use std::sync::Arc;

use ponzik_i18n::{tr, tr_fmt, tr_fmt_n, tr_noop};
use ponzik_engine::effects::mix_assistant::{self as ma, Analysis, MixShared, N_BANDS, Profile, Suggestion};
use ponzik_engine::{Command, Engine, ParamInfo, Unit};
use ponzik_ui::{Canvas, Rect};
use ponzik_window::Cursor;

use super::Input;
use super::app::{App, Popup};
use super::devices::{Device, device_target};
use super::theme::*;
use super::visuals::{device_key, filled_curve, fmt_db1, freq_grid, freq_of_x, x_of_freq};
use super::widgets::wid;

/// Largeur de la carte de l'assistant.
pub const WIDTH: f32 = 1_080.0;
const GRAPH_W: f32 = 430.0;
const SIDE_W: f32 = 150.0;
const GAP: f32 = 16.0;
const CELL_W: f32 = 72.0;
const CELL_H: f32 = 76.0;
const MODULE_H: f32 = 38.0;
const MODULE_GAP: f32 = 6.0;
/// Échelle de l'EQ (±dB sur la demi-hauteur) et étendue du spectre (dB).
const EQ_DB: f32 = 15.0;
const SPAN_DB: f32 = 54.0;
/// Échelle des indicateurs de réduction (dB).
const GR_DB: f32 = 12.0;

/// Modules dans l'ordre des cartes : nom, interrupteur, aide.
const MODULES: [(&str, Option<u32>, &str); 8] = [
    (tr_noop("High-pass"), Some(ma::P_HP_ON), tr_noop("High-pass\nRemoves rumble below the useful range")),
    (tr_noop("EQ"), Some(ma::P_EQ_ON), tr_noop("EQ\nLow shelf, three bells, high shelf")),
    (tr_noop("De-esser"), Some(ma::P_DS_ON), tr_noop("De-esser\nTames harsh \"s\" sounds above its frequency")),
    (tr_noop("Compressor"), Some(ma::P_COMP_ON), tr_noop("Compressor\nEvens out the level")),
    (tr_noop("Saturation"), Some(ma::P_SAT_ON), tr_noop("Saturation\nAdds warmth and density")),
    (tr_noop("Width"), Some(ma::P_WIDTH_ON), tr_noop("Stereo width\nNarrows or widens the side signal")),
    (tr_noop("Output"), None, tr_noop("Output\nLevel after the chain")),
    (tr_noop("Analysis"), None, tr_noop("Analysis\nWhat the last listen measured")),
];
const EQ_MODULE: usize = 1;
const ANALYSIS_MODULE: usize = 7;
const BAND_NAMES: [&str; 5] = [tr_noop("Low"), "1", "2", "3", tr_noop("High")];
const BAND_LABELS: [&str; 5] = ["L", "1", "2", "3", "H"];

/// Ce que l'interface retient d'un assistant.
#[derive(Default)]
struct Assist {
    shared: Option<Arc<MixShared>>,
    /// Dernière écoute terminée déjà prise en compte.
    seen: u32,
    analysis: Option<Analysis>,
    /// Une suggestion attend d'être appliquée ou écartée.
    pending: bool,
    module: usize,
    band: usize,
}

thread_local! {
    static ASSIST: RefCell<HashMap<(usize, usize), Assist>> = RefCell::new(HashMap::new());
}

fn with<R>(key: (usize, usize), f: impl FnOnce(&mut Assist) -> R) -> R {
    ASSIST.with(|m| f(m.borrow_mut().entry(key).or_default()))
}

/// Retient l'état partagé des assistants envoyés au moteur ; à appeler sur chaque
/// commande avant de l'envoyer.
pub fn capture(cmd: &Command) {
    let Command::SetInsert { track, slot, effect } = cmd else { return };
    let shared = effect.as_ref().and_then(|e| e.shared()).and_then(|s| s.downcast::<MixShared>().ok());
    ASSIST.with(|m| {
        let mut m = m.borrow_mut();
        match shared {
            Some(s) => {
                // Nouvelle instance : nouvelle écoute à faire, le choix du module reste.
                let a = m.entry((*track, *slot)).or_default();
                *a = Assist { shared: Some(s), module: a.module, band: a.band, ..Assist::default() };
            }
            None => {
                m.remove(&(*track, *slot));
            }
        }
    });
}

/// Nombre au séparateur décimal de la langue, vrai signe moins.
fn num(v: f32, decimals: usize) -> String {
    let s = ponzik_i18n::num(v.abs() as f64, decimals);
    if v < 0.0 && s.chars().any(|c| c.is_ascii_digit() && c != '0') { format!("−{s}") } else { s }
}

/// Réglages proposés en clair, pour les infobulles.
fn describe(s: &Suggestion, module: usize) -> String {
    let p = ma::params();
    let f = |id: u32| p[id as usize].format(s.get(id));
    match module {
        0 if s.get(ma::P_HP_ON) > 0.5 => tr_fmt!("High-pass at {}", f(ma::P_HP_FREQ)),
        0 => tr("No high-pass").into(),
        1 => {
            let bands: Vec<String> = (0..ma::EQ_BANDS as u32)
                .map(|b| ma::P_BAND + 3 * b)
                .filter(|&id| s.get(id + 1) != 0.0)
                .map(|id| tr_fmt!("{} at {}", f(id + 1), f(id)))
                .collect();
            if bands.is_empty() { tr("Flat EQ").into() } else { bands.join(", ") }
        }
        2 if s.get(ma::P_DS_RANGE) > 0.0 => {
            tr_fmt!("Up to {} above {}, threshold {}", f(ma::P_DS_RANGE), f(ma::P_DS_FREQ), f(ma::P_DS_THRESHOLD))
        }
        2 => tr("No de-essing").into(),
        3 => tr_fmt!(
            "{} from {}, attack {}, release {}, makeup {}",
            f(ma::P_COMP_RATIO),
            f(ma::P_COMP_THRESHOLD),
            f(ma::P_COMP_ATTACK),
            f(ma::P_COMP_RELEASE),
            f(ma::P_COMP_MAKEUP)
        ),
        4 => tr_fmt!("Drive {}", f(ma::P_SAT_DRIVE)),
        5 => tr_fmt!("Width {}", f(ma::P_WIDTH)),
        6 => tr_fmt!("Output {}", f(ma::P_OUTPUT)),
        _ => String::new(),
    }
}

/// Paramètres réglés par les potards d'un module (bande d'EQ choisie pour l'EQ).
fn module_knobs(module: usize, band: usize) -> Vec<u32> {
    match module {
        0 => vec![ma::P_HP_FREQ],
        1 => {
            let id = ma::P_BAND + 3 * band as u32;
            vec![id, id + 1, id + 2]
        }
        2 => vec![ma::P_DS_FREQ, ma::P_DS_THRESHOLD, ma::P_DS_RANGE],
        3 => vec![ma::P_COMP_THRESHOLD, ma::P_COMP_RATIO, ma::P_COMP_ATTACK, ma::P_COMP_RELEASE, ma::P_COMP_MAKEUP],
        4 => vec![ma::P_SAT_DRIVE],
        5 => vec![ma::P_WIDTH],
        6 => vec![ma::P_OUTPUT],
        _ => Vec::new(),
    }
}

/// Nom court sous un potard : le module ou la bande est déjà nommé à côté.
fn knob_name(id: u32) -> &'static str {
    match id {
        ma::P_HP_FREQ => tr("Cutoff"),
        id if (ma::P_BAND..ma::P_BAND + 3 * ma::EQ_BANDS as u32).contains(&id) => [tr("Freq"), tr("Gain"), "Q"][((id - ma::P_BAND) % 3) as usize],
        ma::P_DS_FREQ => tr("Freq"),
        ma::P_DS_THRESHOLD => tr("Threshold"),
        ma::P_DS_RANGE => tr("Range"),
        id => tr(ma::params()[id as usize].name),
    }
}

/// Paramètres d'un module, interrupteur compris (pour savoir si la suggestion les change).
fn module_ids(module: usize) -> Vec<u32> {
    let mut ids: Vec<u32> = match module {
        1 => (0..ma::EQ_BANDS).flat_map(|b| module_knobs(1, b)).collect(),
        m => module_knobs(m, 0),
    };
    ids.extend(MODULES[module].1);
    ids
}

impl App {
    fn ma_value(&self, device: Device, id: u32) -> f32 {
        self.param_value(device, &ma::params()[id as usize])
    }

    fn ma_set(&mut self, device: Device, id: u32, v: f32) {
        self.set_param_value(device, &ma::params()[id as usize], v);
    }

    /// État partagé de l'assistant ; s'il manque (projet ouvert au démarrage),
    /// l'effet est recréé avec les mêmes réglages pour le retrouver.
    fn assist_shared(&mut self, track: usize, slot: usize) -> Option<Arc<MixShared>> {
        if let Some(s) = with((track, slot), |a| a.shared.clone()) {
            return Some(s);
        }
        let def = self.project.tracks[track].inserts[slot].clone()?;
        let fx = ponzik_project::Project::make_effect(&def, self.sample_rate as f32, self.project.bpm)?;
        let cmd = Command::SetInsert { track, slot, effect: Some(fx) };
        capture(&cmd);
        self.send(cmd);
        with((track, slot), |a| a.shared.clone())
    }

    /// Écrit la suggestion dans les réglages : une seule étape d'annulation.
    fn apply_suggestion(&mut self, track: usize, slot: usize, s: &Suggestion) {
        if let Some(fx) = self.project.tracks[track].inserts[slot].as_mut() {
            for &(id, v) in &s.values {
                fx.set(id, v);
            }
        }
        self.commit(tr("Apply mix suggestion"));
        self.toast(&tr_fmt!("Applied the {} suggestion · Ctrl+Z to undo", tr(s.profile.name())));
    }

    pub(super) fn mix_assistant_device(&mut self, c: &mut Canvas, input: &Input, body: Rect, device: Device) {
        let Device::Effect { track, slot } = device else { return };
        let key = (track, slot);
        let shared = self.assist_shared(track, slot);

        // Écoute terminée : on garde le résultat, on arrête l'écoute, la suggestion attend.
        if let Some(sh) = &shared {
            let generation = sh.generation();
            let fresh = with(key, |a| {
                if generation > a.seen {
                    a.seen = generation;
                    a.analysis = sh.analysis();
                    a.pending = a.analysis.is_some();
                    a.pending
                } else {
                    false
                }
            });
            if fresh && self.ma_value(device, ma::P_LISTEN) > 0.5 {
                self.ma_set(device, ma::P_LISTEN, 0.0);
            }
        }
        let (analysis, pending) = with(key, |a| (a.analysis, a.pending));
        let requested = Profile::from_index(self.ma_value(device, ma::P_PROFILE).round() as usize);
        let suggestion = analysis.map(|a| ma::suggest(&a, requested));
        let shown = suggestion.filter(|_| pending);

        let graph = Rect::new(body.x, body.y + 4.0, GRAPH_W, body.h - 8.0);
        self.ma_graph(c, input, graph, device, shared.as_deref(), analysis, suggestion, shown);
        let side = Rect::new(graph.right() + GAP, body.y + 4.0, SIDE_W, body.h - 8.0);
        self.ma_side(c, input, side, device, shared.as_deref(), analysis, shown);
        let x = side.right() + GAP;
        let modules = Rect::new(x, body.y + 4.0, body.right() - x, body.h - 8.0);
        self.ma_modules(c, input, modules, device, shared.as_deref(), analysis, shown);
    }

    /// Spectre (entrée en direct, mesure de l'écoute, cible du profil), EQ en place
    /// avec ses poignées et EQ suggérée ; anneau de progression pendant l'écoute.
    #[allow(clippy::too_many_arguments)]
    fn ma_graph(
        &mut self,
        c: &mut Canvas,
        input: &Input,
        g: Rect,
        device: Device,
        shared: Option<&MixShared>,
        analysis: Option<Analysis>,
        suggestion: Option<Suggestion>,
        shown: Option<Suggestion>,
    ) {
        let t = self.theme;
        let dk = device_key(device);
        let sr = self.sample_rate as f32;
        c.fill_rrect(g, 10.0, t.well);
        let prev = c.push_clip(g);
        freq_grid(self, c, g, EQ_DB, &[-6.0, 0.0, 6.0]);

        // Spectre : le haut suit le plus fort de ce qu'on montre.
        let live = shared.map(|s| s.live());
        let nyquist = sr * 0.5;
        let usable = |i: usize| ma::band_center(i) < nyquist * 0.98;
        let loudest = live
            .iter()
            .chain(analysis.as_ref().map(|a| &a.bands))
            .flat_map(|b| (0..N_BANDS).filter(|&i| usable(i)).map(move |i| b[i]))
            .fold(-60.0f32, f32::max);
        let top = self.anim.to(wid("ma-top", dk, 0), loudest + 6.0, 0.4);
        let y_s = |d: f32| g.y + 12.0 + ((top - d) / SPAN_DB).clamp(0.0, 1.1) * (g.h - 24.0);
        let curve = |bands: &[f32; N_BANDS]| -> Vec<(f32, f32)> {
            (0..N_BANDS).filter(|&i| usable(i)).map(|i| (x_of_freq(g, ma::band_center(i)), y_s(bands[i]))).collect()
        };
        if let Some(live) = &live {
            filled_curve(c, &curve(live), g.bottom(), t.text2.alpha(0.06), t.text2.alpha(0.3));
        }
        if let (Some(a), Some(s)) = (analysis, suggestion) {
            // Cible recalée sur la mesure, comme pour la suggestion.
            let mut diffs: Vec<f32> = (0..N_BANDS)
                .filter(|&i| usable(i) && (150.0..5_000.0).contains(&ma::band_center(i)))
                .map(|i| a.bands[i] - ma::target_db(s.profile, ma::band_center(i)))
                .collect();
            diffs.sort_by(f32::total_cmp);
            let offset = diffs.get(diffs.len() / 2).copied().unwrap_or(0.0);
            let target: Vec<(f32, f32)> = (0..=96)
                .map(|k| {
                    let x = g.x + g.w * k as f32 / 96.0;
                    (x, y_s(ma::target_db(s.profile, freq_of_x(g, x)) + offset))
                })
                .collect();
            c.stroke_polyline(&target, 1.4, t.select.alpha(0.75));
            c.stroke_polyline(&curve(&a.bands), 1.6, t.text.alpha(0.8));
        }

        // EQ en place (au dosage courant) et suggérée.
        let mid = g.y + g.h * 0.5;
        let y_e = |d: f32| mid - d.clamp(-EQ_DB * 1.2, EQ_DB * 1.2) / EQ_DB * (g.h * 0.5 - 6.0);
        let amount = self.ma_value(device, ma::P_AMOUNT);
        let values: Vec<f32> = ma::params().iter().map(|p| self.param_value(device, p)).collect();
        let get = |id: u32| values[id as usize];
        let steps = (g.w / 3.0) as usize;
        let eq_pts = |get: &dyn Fn(u32) -> f32| -> Vec<(f32, f32)> {
            (0..=steps)
                .map(|k| {
                    let x = g.x + k as f32 * g.w / steps as f32;
                    (x, y_e(ma::chain_response_db(get, amount, freq_of_x(g, x), sr)))
                })
                .collect()
        };
        let current = eq_pts(&get);
        filled_curve(c, &current, mid, t.accent.alpha(0.12), t.accent);
        if let Some(s) = shown {
            let get_s = |id: u32| if (ma::P_HP_ON..=ma::P_OUTPUT).contains(&id) { s.get(id) } else { values[id as usize] };
            let pts = eq_pts(&get_s);
            // Tirets : la suggestion n'est pas encore appliquée.
            for seg in pts.chunks(4).step_by(2) {
                c.stroke_polyline(seg, 2.0, t.accent_hot.alpha(0.9));
            }
        }

        // Poignées des bandes : glisser = fréquence et gain, molette = Q.
        let selected_band = with(key_of(device), |a| (a.module == EQ_MODULE).then_some(a.band));
        for b in 0..ma::EQ_BANDS {
            let id = ma::P_BAND + 3 * b as u32;
            let (hz, gain) = (get(id), get(id + 1));
            let (hx, hy) = (x_of_freq(g, hz), y_e(gain));
            let hid = wid("ma-h", dk, b);
            let hov = self.hovered(input, hid, Rect::new(hx - 10.0, hy - 10.0, 20.0, 20.0));
            if hov && input.pressed {
                with(key_of(device), |a| {
                    a.module = EQ_MODULE;
                    a.band = b;
                });
                if input.double {
                    self.ma_set(device, id + 1, 0.0);
                } else {
                    self.begin_drag(hid, input, 0.0);
                }
            }
            if self.is_dragging(hid) {
                self.ma_set(device, id, freq_of_x(g, input.x));
                let d = (mid - input.y) / (g.h * 0.5 - 6.0) * EQ_DB;
                self.ma_set(device, id + 1, (d * 10.0).round() / 10.0);
            }
            if hov && input.wheel != 0.0 {
                self.ma_set(device, id + 2, get(id + 2) * 1.15f32.powf(input.wheel));
            }
            if hov {
                self.set_tip(&tr_fmt!("{} band\nDrag: frequency and gain · wheel: Q · double-click: flat", tr(BAND_NAMES[b])), "");
            }
            let active = hov || self.is_dragging(hid) || selected_band == Some(b);
            let hv = self.hover_anim(hid, active);
            let col = if selected_band == Some(b) { t.select } else if gain != 0.0 { t.text } else { t.muted };
            c.fill_circle(hx, hy, 7.0 + hv * 2.0, t.bg_deep.alpha(0.85));
            c.stroke_circle(hx, hy, 7.0 + hv * 2.0, 1.4, col);
            self.text_center(c, self.f.mono_med, 8.0, hx, hy, BAND_LABELS[b], col);
        }
        if get(ma::P_HP_ON) > 0.5 && amount > 0.0 {
            let x = x_of_freq(g, 20.0 * (get(ma::P_HP_FREQ) / 20.0).powf(amount)).round();
            c.fill_rect(Rect::new(x, g.y, 1.0, g.h), t.accent.alpha(0.5));
        }

        // Légende.
        let mut lx = g.x + 10.0;
        let ly = g.y + 10.0;
        let mut legend: Vec<(ponzik_ui::Color, String)> = vec![(t.text2.alpha(0.6), tr("Input").into())];
        if let Some(s) = suggestion {
            let auto = if self.ma_value(device, ma::P_PROFILE) < 0.5 { tr(" (auto)") } else { "" };
            legend.push((t.select, tr_fmt!("Target · {}{}", tr(s.profile.name()), auto)));
            legend.push((t.text.alpha(0.8), tr("Listened").into()));
        }
        legend.push((t.accent, "EQ".into()));
        if shown.is_some() {
            legend.push((t.accent_hot, tr("Suggested").into()));
        }
        for (col, label) in legend {
            c.fill_rect(Rect::new(lx, ly - 1.0, 10.0, 2.0), col);
            lx += 14.0 + self.text(c, self.f.sans, 10.0, lx + 14.0, ly, &label, t.muted) + 10.0;
        }

        // Écoute en cours : anneau de progression.
        let listening = shared.is_some_and(MixShared::listening);
        let fade = self.anim.to(wid("ma-listen", dk, 0), listening as u8 as f32, T_LAYOUT * 2.0);
        if fade > 0.01 {
            let progress = shared.map_or(0.0, MixShared::progress);
            let p = self.anim.to(wid("ma-prog", dk, 0), progress, 0.15);
            c.fill_rrect(g, 10.0, t.well.alpha(0.7 * fade));
            let (cx, cy) = (g.x + g.w * 0.5, g.y + g.h * 0.5 - 10.0);
            let radius = (g.h * 0.28).min(44.0);
            c.stroke_circle(cx, cy, radius, 6.0, t.raised_hi.alpha(fade));
            if p > 0.002 {
                c.stroke_arc(cx, cy, radius, -PI * 0.5, -PI * 0.5 + TAU * p, 6.0, t.accent.alpha(fade));
            }
            let secs = format!("{} s", (p * ma::LISTEN_SECONDS).floor() as u32);
            self.text_center(c, self.f.mono_med, 18.0, cx, cy, &secs, t.text.alpha(fade));
            let (title, hint) = if self.playing {
                (tr("Listening"), tr_fmt!("{} s of audio, any part of the track", ma::LISTEN_SECONDS as u32))
            } else {
                (tr("Waiting for audio"), tr("Start playback to listen").to_string())
            };
            self.text_center(c, self.f.sans_med, 13.0, cx, cy + radius + 16.0, title, t.text.alpha(fade));
            self.text_center(c, self.f.sans, 11.0, cx, cy + radius + 32.0, &hint, t.muted.alpha(fade));
        } else if analysis.is_none() {
            let (cx, cy) = (g.x + g.w * 0.5, g.y + g.h * 0.5);
            c.fill_rrect(Rect::new(cx - 150.0, cy - 24.0, 300.0, 46.0), 10.0, t.bg_deep.alpha(0.92));
            self.text_center(c, self.f.sans_med, 13.0, cx, cy - 8.0, tr("Press Listen while the track plays"), t.text);
            self.text_center(c, self.f.sans, 11.0, cx, cy + 10.0, tr("Nothing changes until you apply"), t.muted);
        }
        c.set_clip(Some(prev));
    }

    /// Écoute, profil, dosage, application de la suggestion.
    #[allow(clippy::too_many_arguments)]
    fn ma_side(
        &mut self,
        c: &mut Canvas,
        input: &Input,
        r: Rect,
        device: Device,
        shared: Option<&MixShared>,
        analysis: Option<Analysis>,
        shown: Option<Suggestion>,
    ) {
        let t = self.theme;
        let dk = device_key(device);
        let Device::Effect { track, slot } = device else { return };

        // Écouter / arrêter.
        let listening = shared.is_some_and(MixShared::listening) || self.ma_value(device, ma::P_LISTEN) > 0.5;
        let button = Rect::new(r.x, r.y, r.w, 32.0);
        let id = wid("ma-listen-btn", dk, 0);
        let hov = self.hovered(input, id, button);
        let hv = self.hover_anim(id, hov);
        let on = self.anim.to(id ^ 0x4f4e, listening as u8 as f32, T_HOVER);
        c.fill_rrect(button, 16.0, t.raised.mix(t.raised_hi, hv).mix(t.accent, on * 0.9));
        let ink = t.text.mix(t.on_accent, on);
        let (bx, by) = (button.x + 18.0, button.y + 16.0);
        if listening {
            let p = shared.map_or(0.0, MixShared::progress);
            c.stroke_circle(bx, by, 6.0, 2.0, ink.alpha(0.3));
            c.stroke_arc(bx, by, 6.0, -PI * 0.5, -PI * 0.5 + TAU * p.max(0.02), 2.0, ink);
        } else {
            c.stroke_circle(bx, by, 6.0, 1.6, ink);
            c.fill_circle(bx, by, 2.5, ink);
        }
        let label = if listening {
            tr_fmt!("Listening {}%", (shared.map_or(0.0, MixShared::progress) * 100.0).round() as u32)
        } else if analysis.is_some() {
            tr("Listen again").to_string()
        } else {
            tr("Listen").to_string()
        };
        self.text(c, self.f.sans_med, 13.0, bx + 14.0, by, &label, ink);
        if hov {
            let tip = if listening { tr("Stop listening") } else { tr("Listen\nAnalyses 10 s of the playing track, then suggests settings") };
            self.set_tip(tip, "");
            self.set_cursor(Cursor::Hand);
            if input.pressed {
                self.ma_set(device, ma::P_LISTEN, if listening { 0.0 } else { 1.0 });
            }
        }

        // Profil de source.
        let info = &ma::params()[ma::P_PROFILE as usize];
        let cell = Rect::new(r.x, r.y + 40.0, r.w, 54.0);
        let value = self.param_value(device, info);
        if let Some(v) = self.choice(c, input, wid("ma-prof", dk, 0), cell, info, value) {
            self.set_param_value(device, info, v);
        }
        self.tip_zone(input, Rect::new(cell.x, cell.y, cell.w, 18.0), tr("Profile\nWhat the track is: sets the target curve and the starting points"), "");

        // Dosage : de neutre à la suggestion appliquée.
        let info = &ma::params()[ma::P_AMOUNT as usize];
        let amount = self.param_value(device, info);
        let slider = Rect::new(r.x, r.y + 100.0, r.w, 28.0);
        let id = wid("ma-amount", dk, 0);
        let hov = self.hovered(input, id, slider);
        if hov && input.pressed {
            if input.double {
                self.set_param_value(device, info, 1.0);
            } else {
                self.begin_drag(id, input, amount);
                self.drag = self.drag.map(|mut d| {
                    d.y0 = input.x;
                    d
                });
            }
        }
        if let Some(d) = self.drag.filter(|d| d.id == id) {
            let k = if d.fine { 0.2 } else { 1.0 };
            self.set_param_value(device, info, d.v0 + (input.x - d.y0) / slider.w * k);
        }
        if hov && input.wheel != 0.0 {
            self.set_param_value(device, info, amount + 0.05 * input.wheel);
        }
        let hv = self.hover_anim(id, hov || self.is_dragging(id));
        if hov {
            self.set_tip(tr("Amount\nFrom neutral (0 %, untouched sound) to the full settings · double-click: 100 %"), "");
            self.set_cursor(Cursor::ResizeH);
        }
        self.text(c, self.f.sans, 12.0, slider.x, slider.y + 6.0, tr("Amount"), t.text2.mix(t.text, hv));
        self.text_right(c, self.f.mono, 11.0, slider.right(), slider.y + 6.0, &info.format(amount), t.text);
        let bar = Rect::new(slider.x, slider.y + 17.0, slider.w, 6.0);
        c.fill_rrect(bar, 3.0, t.raised_hi);
        c.fill_rrect(Rect::new(bar.x, bar.y, (bar.w * amount).max(6.0), bar.h), 3.0, t.accent.alpha(if amount > 0.0 { 1.0 } else { 0.3 }));
        c.fill_circle(bar.x + bar.w * amount.clamp(0.02, 0.98), bar.y + 3.0, 5.0 + hv, t.text);

        // Suggestion : appliquer, écarter ou la revoir.
        let row = Rect::new(r.x, r.y + 136.0, r.w, 28.0);
        if let Some(s) = shown {
            let apply = Rect::new(row.x, row.y, row.w - 34.0, row.h);
            let id = wid("ma-apply", dk, 0);
            let hov = self.hovered(input, id, apply);
            let hv = self.hover_anim(id, hov);
            c.fill_rrect(apply, 14.0, t.accent.mix(t.accent_hot, hv * 0.3));
            let (cx, cy) = apply.center();
            self.text_center(c, self.f.sans_med, 12.0, cx, cy, tr("Apply suggestion"), t.on_accent);
            if hov {
                self.set_tip(&tr_fmt!("Apply the {} suggestion\nEvery value stays editable · Ctrl+Z undoes it", s.profile.name()), "");
                self.set_cursor(Cursor::Hand);
                if input.pressed {
                    self.apply_suggestion(track, slot, &s);
                    with((track, slot), |a| a.pending = false);
                }
            }
            let close = Rect::new(row.right() - 28.0, row.y, 28.0, 28.0);
            if self.icon_button(c, input, wid("ma-dismiss", dk, 0), close, super::widgets::Icon::Close, false, (tr("Dismiss the suggestion"), "")) {
                with((track, slot), |a| a.pending = false);
            }
        } else if let Some(a) = analysis {
            let id = wid("ma-show", dk, 0);
            let hov = self.hovered(input, id, row);
            let hv = self.hover_anim(id, hov);
            c.stroke_rrect(row, 14.0, 1.0, t.hairline.alpha(1.0 + hv * 2.0));
            let (cx, cy) = row.center();
            self.text_center(c, self.f.sans, 12.0, cx, cy, tr("Show suggestion"), t.text2.mix(t.text, hv));
            if hov {
                self.set_tip(&tr_fmt!("Suggestion from the last listen ({} s)", num(a.seconds, 0)), "");
                self.set_cursor(Cursor::Hand);
                if input.pressed {
                    with((track, slot), |a| a.pending = true);
                }
            }
        }
    }

    /// Cartes des modules (interrupteur, valeur, indicateur) et potards du module choisi.
    #[allow(clippy::too_many_arguments)]
    fn ma_modules(
        &mut self,
        c: &mut Canvas,
        input: &Input,
        r: Rect,
        device: Device,
        shared: Option<&MixShared>,
        analysis: Option<Analysis>,
        shown: Option<Suggestion>,
    ) {
        let t = self.theme;
        let dk = device_key(device);
        let key = key_of(device);
        let p = ma::params();
        let (mut module, band) = with(key, |a| (a.module, a.band));
        let card_w = (r.w - 3.0 * MODULE_GAP) / 4.0;

        // Réductions de gain : montée immédiate, retombée douce.
        let meter = |app: &mut App, tag: &str, now: f32| {
            let id = wid(tag, dk, 0);
            let prev = app.anim.get(id).unwrap_or(0.0);
            let v = if now > prev { now } else { prev + (now - prev) * 0.12 };
            app.anim.set(id, v);
            v
        };
        let comp_gr = meter(self, "ma-comp-gr", shared.map_or(0.0, MixShared::compressor_gr));
        let ds_gr = meter(self, "ma-ds-gr", shared.map_or(0.0, MixShared::deesser_gr));

        for (i, &(name, switch, help)) in MODULES.iter().enumerate() {
            let (name, help) = (tr(name), tr(help));
            let card = Rect::new(
                r.x + (i % 4) as f32 * (card_w + MODULE_GAP),
                r.y + (i / 4) as f32 * (MODULE_H + MODULE_GAP),
                card_w,
                MODULE_H,
            );
            let on = switch.is_none_or(|id| self.ma_value(device, id) > 0.5);
            let id = wid("ma-mod", dk, i);
            let hov = self.hovered(input, id, card);
            let hv = self.hover_anim(id, hov);
            let sel = module == i;
            c.fill_rrect(card, 9.0, t.raised.mix(t.raised_hi, (hv * 0.6).max(sel as u8 as f32 * 0.7)));
            if sel {
                c.stroke_rrect(card, 9.0, 1.2, t.select.alpha(0.8));
            }
            // Interrupteur du module.
            let mut toggled = false;
            if let Some(sw) = switch {
                let dot = Rect::new(card.x + 4.0, card.y + 3.0, 18.0, 18.0);
                let did = wid("ma-sw", dk, i);
                let dhov = self.hovered(input, did, dot);
                let (cx, cy) = dot.center();
                c.fill_circle(cx, cy, 4.0, if on { t.accent } else { t.raised_hi });
                c.stroke_circle(cx, cy, 4.0, 1.0, if dhov { t.text } else { t.hairline.alpha(3.0) });
                if dhov {
                    self.set_tip(&tr_fmt!("{}\nClick: {}", name, if on { tr("bypass") } else { tr("enable") }), "");
                    if input.pressed {
                        self.ma_set(device, sw, if on { 0.0 } else { 1.0 });
                        toggled = true;
                    }
                }
            }
            let ink = if on { t.text } else { t.muted };
            self.text_fit(c, self.f.sans_med, 11.0, card.x + 22.0, card.y + 12.0, name, card.w - 30.0, ink);
            let value = match i {
                0 if on => p[ma::P_HP_FREQ as usize].format(self.ma_value(device, ma::P_HP_FREQ)),
                1 => {
                    let n = (0..ma::EQ_BANDS as u32).filter(|b| self.ma_value(device, ma::P_BAND + 3 * b + 1) != 0.0).count();
                    match n {
                        0 => tr("Flat").into(),
                        n => tr_fmt_n!("{} active band", "{} active bands", n, n),
                    }
                }
                2 if self.ma_value(device, ma::P_DS_RANGE) > 0.0 => p[ma::P_DS_FREQ as usize].format(self.ma_value(device, ma::P_DS_FREQ)),
                2 => tr("Idle").into(),
                3 if self.ma_value(device, ma::P_COMP_RATIO) > 1.0 => {
                    p[ma::P_COMP_RATIO as usize].format(self.ma_value(device, ma::P_COMP_RATIO))
                }
                3 => tr("Idle").into(),
                4 => p[ma::P_SAT_DRIVE as usize].format(self.ma_value(device, ma::P_SAT_DRIVE)),
                5 => p[ma::P_WIDTH as usize].format(self.ma_value(device, ma::P_WIDTH)),
                6 => p[ma::P_OUTPUT as usize].format(self.ma_value(device, ma::P_OUTPUT)),
                7 => analysis.map_or(tr("Not yet").into(), |a| format!("{} dBFS", num(a.rms_db, 1))),
                _ => tr("Off").into(),
            };
            self.text(c, self.f.mono, 10.0, card.x + 10.0, card.y + 28.0, &value, if on { t.text2 } else { t.muted });
            // Indicateur de réduction (compresseur, de-esser).
            let gr = match i {
                2 => Some(ds_gr),
                3 => Some(comp_gr),
                _ => None,
            };
            if let Some(gr) = gr {
                let bar = Rect::new(card.right() - 44.0, card.y + 25.0, 36.0, 5.0);
                c.fill_rrect(bar, 2.5, t.bg_deep);
                let w = (gr / GR_DB).clamp(0.0, 1.0) * bar.w;
                if w > 0.5 && on {
                    c.fill_rrect(Rect::new(bar.right() - w, bar.y, w, bar.h), 2.5, t.accent);
                }
            }
            // La suggestion changerait ce module : un point sarcelle.
            let changes = shown.filter(|s| module_ids(i).iter().any(|&id| (s.get(id) - self.ma_value(device, id)).abs() > 1e-3));
            if let Some(s) = changes {
                c.fill_circle(card.right() - 9.0, card.y + 10.0, 3.0, t.select);
                if hov {
                    self.set_tip(&tr_fmt!("{}\nSuggested: {}", name, describe(&s, i)), "");
                }
            } else if hov {
                let tip = match (i, gr) {
                    (_, Some(gr)) => tr_fmt!("{}\nReduction now: {} dB", help, fmt_db1(-gr)),
                    _ => help.to_string(),
                };
                self.set_tip(&tip, "");
            }
            if hov && input.pressed && !toggled {
                module = i;
                with(key, |a| a.module = i);
            }
        }

        // Réglages du module choisi.
        let y = r.y + 2.0 * (MODULE_H + MODULE_GAP) + 4.0;
        let mut x = r.x;
        c.fill_rect(Rect::new(r.x, y - 4.0, r.w, 1.0), t.hairline);
        if module == EQ_MODULE {
            let seg = Rect::new(x, y + 22.0, 150.0, 30.0);
            let tips: Vec<(&str, &str)> = BAND_NAMES.iter().map(|_| (tr("Band to edit"), "")).collect();
            if let Some(b) = self.segmented_tips(c, input, wid("ma-band", dk, 0), seg, &BAND_LABELS, &tips, band) {
                with(key, |a| a.band = b);
            }
            self.text(c, self.f.sans, 12.0, x + 2.0, y + 8.0, &tr_fmt!("{} band", tr(BAND_NAMES[band])), t.text2);
            x += 160.0;
        }
        if module == ANALYSIS_MODULE {
            self.ma_readings(c, Rect::new(x, y, r.right() - x, r.bottom() - y), analysis);
            return;
        }
        for pid in module_knobs(module, band) {
            let info: &ParamInfo = &p[pid as usize];
            let label = ParamInfo { name: knob_name(pid), ..*info };
            let cell = Rect::new(x, y, CELL_W, CELL_H);
            let value = self.param_value(device, info);
            let changed = if info.unit == Unit::Choice {
                self.choice(c, input, wid("ma-prm", dk, pid as usize), cell, &label, value)
            } else {
                self.knob(c, input, wid("ma-prm", dk, pid as usize), cell, &label, value)
            };
            if let Some(v) = changed {
                self.set_param_value(device, info, v);
            }
            // Repère de la valeur suggérée sur l'arc du potard.
            if let Some(s) = shown
                && (s.get(pid) - value).abs() > 1e-3
            {
                let a = 0.75 * PI + 1.5 * PI * info.to_norm(s.get(pid));
                let (cx, cy) = (cell.x + cell.w * 0.5, cell.y + 34.0);
                c.fill_circle(cx + a.cos() * 23.0, cy + a.sin() * 23.0, 2.5, t.select);
            }
            let target = device_target(device, pid);
            if input.right_pressed && input.inside && cell.contains(input.x, input.y) && self.drag.is_none() {
                let popup = Popup::Param { target, value, default: info.default };
                self.open_popup(popup, Rect::new(input.x, input.y, 0.0, 0.0));
            }
            if self.is_automated(target) {
                c.fill_circle(cell.right() - 10.0, cell.y + 6.0, 3.0, t.select);
            }
            x += CELL_W;
        }
    }

    /// Mesures de la dernière écoute.
    fn ma_readings(&mut self, c: &mut Canvas, r: Rect, analysis: Option<Analysis>) {
        let t = self.theme;
        let Some(a) = analysis else {
            self.text(c, self.f.sans, 12.0, r.x + 2.0, r.y + 16.0, tr("Listen first: level, dynamics, sibilance and stereo image show up here"), t.muted);
            return;
        };
        let stereo = if a.side_db < -30.0 { tr("Mono").to_string() } else { tr_fmt!("{} dB side", num(a.side_db, 0)) };
        let rows = [
            (tr("Level"), format!("{} dBFS", num(a.rms_db, 1))),
            (tr("Peak"), format!("{} dBFS", num(a.peak_db, 1))),
            (tr("Crest"), format!("{} dB", num(a.crest_db, 1))),
            (tr("Dynamics"), format!("{} dB", num(a.range_db, 1))),
            (tr("Sibilance"), format!("{} dB", num(a.sibilance_db, 1))),
            (tr("Image"), stereo),
            (tr("Correlation"), num(a.correlation, 2)),
            (tr("Listened"), format!("{} s", num(a.seconds, 1))),
        ];
        let col_w = (r.w / 2.0).min(200.0);
        for (k, (label, value)) in rows.iter().enumerate() {
            let (x, y) = (r.x + (k / 4) as f32 * col_w, r.y + 10.0 + (k % 4) as f32 * 17.0);
            self.text(c, self.f.sans, 11.0, x + 2.0, y, label, t.muted);
            self.text_right(c, self.f.mono, 11.0, x + col_w - 16.0, y, value, t.text);
        }
    }

    /// Capture d'écran : les assistants écoutent `secs` secondes de lecture ; avec
    /// `apply`, la suggestion est appliquée et le compresseur montré.
    pub(super) fn screenshot_listen(&mut self, engine: &mut Engine, secs: f32, apply: bool) {
        for track in 0..self.project.tracks.len() {
            for slot in 0..self.project.tracks[track].inserts.len() {
                if let Some(fx) = self.project.tracks[track].inserts[slot].as_mut()
                    && fx.kind == "mix_assistant"
                {
                    fx.set(ma::P_LISTEN, 1.0);
                }
            }
        }
        self.commit(tr("Listen"));
        let mut audio = vec![0.0f32; 800 * 2];
        for _ in 0..(secs * self.sample_rate as f32 / 800.0) as usize {
            engine.process(&mut audio, 2);
        }
        if !apply {
            return;
        }
        let keys: Vec<(usize, usize)> = ASSIST.with(|m| m.borrow().keys().copied().collect());
        for (track, slot) in keys {
            let Some(shared) = with((track, slot), |a| a.shared.clone()) else { continue };
            let Some(analysis) = shared.analysis() else { continue };
            let device = Device::Effect { track, slot };
            let profile = Profile::from_index(self.ma_value(device, ma::P_PROFILE).round() as usize);
            self.ma_set(device, ma::P_LISTEN, 0.0);
            self.apply_suggestion(track, slot, &ma::suggest(&analysis, profile));
            with((track, slot), |a| {
                a.seen = shared.generation();
                a.analysis = Some(analysis);
                a.module = 3;
            });
        }
    }
}

fn key_of(device: Device) -> (usize, usize) {
    match device {
        Device::Effect { track, slot } | Device::RackUnit { track, slot, .. } => (track, slot),
        Device::Instrument(ch) => (usize::MAX, ch),
    }
}
