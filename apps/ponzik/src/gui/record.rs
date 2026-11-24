//! Enregistrement audio : entrée en duplex intégral, pistes armées, écoute,
//! prises alignées sur le morceau, calibrage de la latence, et le panneau
//! d'enregistrement (entrée, niveau, gain, écoute, latence).
//!
//! L'entrée est branchée sur le flux de sortie ([`ponzik_audio::OutputStream::set_input`]) :
//! le moteur la reçoit dans le même appel que la sortie, et la voix écoutée
//! repart dans ce même cycle. Les prises restent sèches ; à leur retour, elles
//! sont recalées de toute la latence mesurée (compensation du mixer, sortie,
//! entrée, réglage manuel ou calibré), découpées par tour de boucle et
//! posées à l'échantillon près dans la playlist.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use ponzik_i18n::{tr, tr_fmt, tr_fmt_n};

use ponzik_audio::{DeviceInfo, Latency, OutputStream};
use ponzik_engine::input::{CALIBRATION_SECONDS, clip_placement, measure_round_trip, take_slices};
use ponzik_engine::mixer::MASTER;
use ponzik_engine::{Command, InputSource, InputStrip, PlayMode, RecordBuffer, Sample};
use ponzik_project::{AudioClipDef, BitDepth};
use ponzik_ui::{Canvas, Rect};

use super::Input;
use super::app::App;
use super::compat;
use super::theme::*;
use super::widgets::{Icon, draw_icon, wid};

/// Durée maximale d'une prise, en secondes.
const TAKE_SECONDS: f32 = 600.0;
/// Tampon de départ d'une prise ; elle s'allonge ensuite par morceaux, alloués
/// ici (hors du thread audio) quand il lui reste moins de `TAKE_LOW_SECONDS`.
/// Une prise courte n'occupe plus 230 Mo d'avance.
const TAKE_START_SECONDS: f32 = 15.0;
const TAKE_CHUNK_SECONDS: f32 = 15.0;
const TAKE_LOW_SECONDS: f32 = 8.0;
/// Tampons gardés pour les prochaines prises, ramenés à leur taille de départ.
const SPARE_TAKES: usize = 2;
const PANEL_W: f32 = 372.0;
const PANEL_H: f32 = 556.0;
/// Graduations du VU-mètre d'entrée, en dBFS.
const METER_TICKS: [f32; 7] = [-48.0, -36.0, -24.0, -18.0, -12.0, -6.0, 0.0];
const METER_FLOOR: f32 = -60.0;

/// Réglages d'enregistrement, gardés d'une session à l'autre.
#[derive(Clone, Debug, PartialEq)]
pub struct RecordSettings {
    /// Entrée choisie (`None` : celle du système).
    pub device: Option<String>,
    /// Source des pistes qu'on arme.
    pub source: InputSource,
    pub strip: InputStrip,
    /// Écouter l'entrée sur les pistes armées.
    pub monitor: bool,
    /// Écoute à faible latence (sans compensation ni anticipation sur le trajet).
    pub low_latency: bool,
    /// Décalage ajouté à la latence mesurée, en ms (positif : prises avancées).
    pub offset_ms: f32,
    /// Prises en flottant 32 bits plutôt qu'en 24 bits.
    pub float: bool,
    /// Ne garder que la sélection de temps (punch in/out).
    pub punch: bool,
}

impl Default for RecordSettings {
    fn default() -> Self {
        Self {
            device: None,
            source: InputSource::Left,
            strip: InputStrip::default(),
            monitor: true,
            low_latency: true,
            offset_ms: 0.0,
            float: false,
            punch: true,
        }
    }
}

impl RecordSettings {
    /// Texte `clé=valeur`, une ligne par réglage.
    pub fn to_text(&self) -> String {
        let source = match self.source {
            InputSource::Stereo => "stereo",
            InputSource::Left => "in1",
            InputSource::Right => "in2",
        };
        let s = &self.strip;
        format!(
            "device={}\nsource={source}\ngain_db={}\nhigh_pass={}\ngate={}\ngate_threshold_db={}\nmonitor={}\nlow_latency={}\noffset_ms={}\nfloat={}\npunch={}\n",
            self.device.as_deref().unwrap_or(""),
            s.gain_db,
            s.high_pass,
            s.gate,
            s.gate_threshold_db,
            self.monitor,
            self.low_latency,
            self.offset_ms,
            self.float,
            self.punch
        )
    }

    /// Relit [`RecordSettings::to_text`] ; les lignes inconnues ou invalides gardent le défaut.
    pub fn from_text(text: &str) -> Self {
        let mut s = Self::default();
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let v = v.trim();
            let flag = v == "true";
            let num = v.parse::<f32>().ok().filter(|x| x.is_finite());
            match k.trim() {
                "device" => s.device = (!v.is_empty()).then(|| v.to_string()),
                "source" => {
                    s.source = match v {
                        "stereo" => InputSource::Stereo,
                        "in2" => InputSource::Right,
                        _ => InputSource::Left,
                    }
                }
                "gain_db" => s.strip.gain_db = num.unwrap_or(0.0).clamp(-24.0, 24.0),
                "high_pass" => s.strip.high_pass = flag,
                "gate" => s.strip.gate = flag,
                "gate_threshold_db" => s.strip.gate_threshold_db = num.unwrap_or(-50.0).clamp(-80.0, -10.0),
                "monitor" => s.monitor = flag,
                "low_latency" => s.low_latency = flag,
                "offset_ms" => s.offset_ms = num.unwrap_or(0.0).clamp(-200.0, 200.0),
                "float" => s.float = flag,
                "punch" => s.punch = flag,
                _ => {}
            }
        }
        s
    }
}

/// Fichier des réglages d'enregistrement.
fn settings_path() -> Option<PathBuf> {
    if cfg!(windows) {
        return std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("Ponzik").join("recording.cfg"));
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("ponzik").join("recording.cfg"))
}

/// Résultat du dernier calibrage.
#[derive(Clone, Copy, Debug)]
pub struct Calibration {
    pub measured: u32,
    pub reported: u32,
}

/// Tout l'état de l'enregistrement côté interface.
#[derive(Default)]
pub struct RecordState {
    /// Flux de sortie, sur lequel l'entrée est branchée (absent en capture d'écran).
    pub output: Option<OutputStream>,
    pub settings: RecordSettings,
    /// Réglages relus et sauvegardés sur disque (pas en capture d'écran).
    persist: bool,
    pub devices: Vec<DeviceInfo>,
    /// Entrée ouverte : nom et canaux.
    pub input: Option<(String, u16)>,
    /// Pistes armées et leur source.
    pub armed: Vec<(usize, InputSource)>,
    /// Tampons confiés au moteur : piste et source.
    in_engine: Vec<(usize, InputSource)>,
    /// Prises en cours allongées : piste, trames accordées, moment du dernier envoi.
    granted: Vec<(usize, usize, f32)>,
    /// Tampons revenus, réutilisés (aucune nouvelle allocation de 100 Mo par prise).
    /// Les boîtes repartent telles quelles au moteur.
    #[allow(clippy::vec_box)]
    spare: Vec<Box<RecordBuffer>>,
    /// Numéro de prise par nom de piste.
    counts: HashMap<String, u32>,
    pub panel: bool,
    pub panel_anchor: (f32, f32),
    panel_rect: Option<Rect>,
    device_list: bool,
    /// Dernière erreur d'ouverture de l'entrée : affichée dans le panneau, et on
    /// ne réessaie pas à chaque image tant que rien n'a changé.
    input_error: Option<String>,
    /// VU-mètre d'entrée : niveau, crête maintenue, âge de la crête.
    level: [f32; 2],
    hold: [f32; 2],
    hold_age: f32,
    /// Saturation vue depuis le dernier clic sur le témoin.
    pub clipped: bool,
    gate: f32,
    pub latency: Latency,
    /// Calibrage en cours (instant de départ), dernier résultat.
    calibrating: Option<f32>,
    pub calibration: Option<Calibration>,
    /// Capture d'écran : niveaux simulés, fichiers dans le dossier temporaire.
    pub(super) demo: bool,
}

impl RecordState {
    pub fn is_armed(&self, track: usize) -> bool {
        self.armed.iter().any(|&(t, _)| t == track)
    }

    /// Armed tracks and takes follow their track to its new number; those of
    /// a deleted track are dropped (the engine ends their take).
    pub(super) fn follow_tracks(&mut self, map: impl Fn(usize) -> Option<usize>) {
        self.armed = self.armed.iter().filter_map(|&(t, s)| Some((map(t)?, s))).collect();
        self.in_engine = self.in_engine.iter().filter_map(|&(t, s)| Some((map(t)?, s))).collect();
        self.granted = self.granted.iter().filter_map(|&(t, n, at)| Some((map(t)?, n, at))).collect();
    }

    /// Vrai quand quelque chose bouge (VU-mètre, calibrage).
    pub fn needs_redraw(&self) -> bool {
        (self.input.is_some() && (self.panel || !self.armed.is_empty())) || self.calibrating.is_some()
    }

    /// Niveau d'entrée de 0 à 1 sur l'échelle du VU-mètre (−60 à 0 dBFS).
    pub fn input_level(&self) -> f32 {
        let db = 20.0 * self.level[0].max(self.level[1]).max(1e-6).log10();
        ((db - METER_FLOOR) / -METER_FLOOR).clamp(0.0, 1.0)
    }
}

impl App {
    /// Branche le flux de sortie (duplex) et relit les réglages enregistrés.
    pub fn set_output(&mut self, stream: OutputStream) {
        self.rec.output = Some(stream);
        self.rec.persist = true;
        if let Some(text) = settings_path().and_then(|p| std::fs::read_to_string(p).ok()) {
            self.rec.settings = RecordSettings::from_text(&text);
        }
    }

    fn save_record_settings(&mut self) {
        if !self.rec.persist {
            return;
        }
        if let Some(path) = settings_path() {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(path, self.rec.settings.to_text());
        }
    }

    /// Change un réglage, le transmet au moteur et le garde sur disque.
    fn set_record_settings(&mut self, settings: RecordSettings) {
        let old = std::mem::replace(&mut self.rec.settings, settings.clone());
        if old == settings {
            return;
        }
        if old.strip != settings.strip {
            self.send(Command::SetInputStrip(settings.strip));
        }
        if old.low_latency != settings.low_latency {
            self.send(Command::SetLowLatencyMonitoring(settings.low_latency));
        }
        if old.source != settings.source {
            // Toutes les pistes armées suivent la source choisie.
            for a in &mut self.rec.armed {
                a.1 = settings.source;
            }
        }
        if old.monitor != settings.monitor || old.source != settings.source {
            self.update_monitors();
        }
        if old.device != settings.device {
            self.rec.input_error = None;
            if self.rec.input.is_some() {
                self.close_input();
                self.open_input();
            }
        }
        self.save_record_settings();
    }

    /// Écoute de chaque piste armée selon les réglages.
    pub(super) fn update_monitors(&mut self) {
        let armed = self.rec.armed.clone();
        let on = self.rec.settings.monitor && self.rec.input.is_some();
        for (track, source) in armed {
            self.send(Command::SetMonitor { track, source: on.then_some(source) });
        }
    }

    /// Ouvre l'entrée choisie et la branche sur la sortie. Renvoie faux en cas d'échec.
    pub(super) fn open_input(&mut self) -> bool {
        if self.rec.input.is_some() {
            return true;
        }
        let Some(output) = self.rec.output.as_mut() else { return false };
        let device = self.rec.settings.device.clone();
        let rate = self.sample_rate;
        let mut attach = |id: Option<&str>| {
            ponzik_audio::open_input(id, rate).and_then(|input| {
                let config = input.config();
                let name = input.device_name().to_string();
                output.set_input(Some(input)).map(|()| (name, config))
            })
        };
        let mut opened = attach(device.as_deref());
        // Micro enregistré introuvable ou occupé : on retombe sur celui du système.
        if opened.is_err()
            && let Some(id) = device.as_deref()
            && let Ok(ok) = attach(None)
        {
            opened = Ok(ok);
            self.rec.settings.device = None;
            self.save_record_settings();
            self.toast(&tr_fmt!("Microphone \"{}\" unavailable: using the system default input", id));
        }
        match opened {
            Ok((name, config)) => {
                self.rec.input_error = None;
                if config.sample_rate != self.sample_rate {
                    self.toast(tr("Warning: the input and output run at different sample rates"));
                }
                self.rec.input = Some((name, config.channels));
                let s = self.rec.settings.clone();
                self.send(Command::SetInputStrip(s.strip));
                self.send(Command::SetLowLatencyMonitoring(s.low_latency));
                self.update_monitors();
                true
            }
            Err(e) => {
                self.rec.input_error = Some(tr_fmt!("Can't open the microphone: {}", e.to_string()));
                false
            }
        }
    }

    fn close_input(&mut self) {
        if self.rec.input.take().is_none() {
            return;
        }
        if let Some(output) = self.rec.output.as_mut() {
            let _ = output.set_input(None);
        }
        for (track, _) in self.rec.armed.clone() {
            self.send(Command::SetMonitor { track, source: None });
        }
    }

    /// Arme ou désarme une piste. Le premier armement ouvre l'entrée, active
    /// l'enregistrement du transport et le panneau d'enregistrement.
    pub fn toggle_arm(&mut self, track: usize) {
        if track == MASTER {
            return;
        }
        let name = self.project.tracks[track].name.clone();
        if self.rec.is_armed(track) {
            self.rec.armed.retain(|&(t, _)| t != track);
            self.send(Command::SetMonitor { track, source: None });
            self.toast(&tr_fmt!("{} disarmed", name));
            return;
        }
        let first = self.rec.armed.is_empty();
        self.rec.armed.push((track, self.rec.settings.source));
        if !self.rec.demo && !self.open_input() {
            self.rec.armed.retain(|&(t, _)| t != track);
            return;
        }
        self.update_monitors();
        self.record = true;
        if matches!(self.project.mode, PlayMode::Pattern(_)) {
            self.set_mode(PlayMode::Song);
        }
        if first && !self.rec.panel {
            self.open_record_panel(None);
        }
        self.toast(&tr_fmt!("{} armed: press play to record", name));
    }

    /// Ouvre le panneau d'enregistrement, sous `anchor` (sinon à sa dernière place).
    pub fn open_record_panel(&mut self, anchor: Option<(f32, f32)>) {
        if let Some(a) = anchor {
            self.rec.panel_anchor = a;
        }
        self.rec.panel = true;
        self.voice.panel = false;
        self.rec.device_list = false;
        self.rec.input_error = None;
        if self.rec.devices.is_empty() && !self.rec.demo {
            self.rec.devices = ponzik_audio::input_devices();
        }
    }

    pub fn toggle_record_panel(&mut self, anchor: (f32, f32)) {
        if self.rec.panel {
            self.rec.panel = false;
        } else {
            self.open_record_panel(Some(anchor));
        }
    }

    /// À chaque image : niveaux, latences, tampons du moteur, prises et calibrage revenus.
    pub(super) fn record_tick(&mut self, dt: f32) {
        // L'entrée reste ouverte tant qu'on en a besoin (piste armée, panneau ouvert).
        let want_input = !self.rec.armed.is_empty() || self.rec.panel || self.voice.enabled || self.audio_edit.capture.is_some();
        if want_input && self.rec.input.is_none() && self.rec.output.is_some() && !self.rec.demo && self.rec.input_error.is_none() {
            // Échec : le panneau reste ouvert sur la liste des micros, avec l'erreur ;
            // on ne réessaie qu'au prochain choix de micro ou à la réouverture.
            if !self.open_input() {
                self.rec.armed.clear();
                self.rec.device_list = true;
                self.rec.devices = ponzik_audio::input_devices();
                if !self.rec.panel {
                    self.open_record_panel(None);
                    self.rec.device_list = true;
                }
            }
        } else if !want_input && self.rec.input.is_some() {
            self.close_input();
        }
        if let Some(out) = self.rec.output.as_ref() {
            self.rec.latency = out.latency();
        }
        self.feed_input_meter(dt);
        self.collect_takes();
        self.sync_record_buffers();
        self.collect_calibration();
    }

    fn feed_input_meter(&mut self, dt: f32) {
        let peak = if self.rec.demo {
            let k = (self.time * 2.1).sin().abs() * 0.5 + 0.18;
            [k, k * 0.92]
        } else {
            self.engine.take_input_peak()
        };
        if self.engine.take_input_clip() {
            self.rec.clipped = true;
        }
        self.rec.gate = self.engine.input_gate();
        let r = &mut self.rec;
        for (i, &p) in peak.iter().enumerate() {
            r.level[i] = p.max(r.level[i] * 10f32.powf(-1.5 * dt));
        }
        let top = peak[0].max(peak[1]);
        if top >= r.hold[0] {
            r.hold = [top; 2];
            r.hold_age = 0.0;
        } else {
            r.hold_age += dt;
            if r.hold_age > 1.5 {
                r.hold[0] *= 10f32.powf(-dt);
                r.hold[1] = r.hold[0];
            }
        }
    }

    /// Le moteur enregistre les pistes armées quand l'enregistrement du transport est actif.
    fn sync_record_buffers(&mut self) {
        let want: Vec<(usize, InputSource)> =
            if self.record && (self.rec.input.is_some() || self.rec.demo) { self.rec.armed.clone() } else { Vec::new() };
        let gone: Vec<(usize, InputSource)> = self.rec.in_engine.iter().filter(|a| !want.contains(a)).copied().collect();
        for (track, _) in gone {
            self.rec.in_engine.retain(|&(t, _)| t != track);
            self.send(Command::Record { track, armed: false, buffer: None });
        }
        let sr = self.sample_rate as f32;
        for (track, source) in want {
            if self.rec.in_engine.contains(&(track, source)) || self.rec.in_engine.len() >= ponzik_engine::MAX_RECORDINGS {
                continue;
            }
            let spare = self.rec.spare.iter().position(|b| b.is_mono() == source.is_mono() && b.sample_rate == sr);
            let mut buffer = match spare {
                Some(i) => self.rec.spare.swap_remove(i),
                None => Box::new(RecordBuffer::for_source(source, if self.rec.demo { 1.0 } else { TAKE_START_SECONDS }, sr)),
            };
            buffer.source = source;
            buffer.len = 0;
            self.rec.in_engine.push((track, source));
            self.rec.granted.retain(|g| g.0 != track);
            self.rec.granted.push((track, buffer.capacity(), self.time));
            self.send(Command::Record { track, armed: true, buffer: Some(buffer) });
        }
        self.feed_takes();
    }

    /// Allonge les prises qui vont manquer de place : un morceau de
    /// `TAKE_CHUNK_SECONDS`, écrit ici, part au moteur bien avant qu'il en ait besoin.
    fn feed_takes(&mut self) {
        if self.rec.demo {
            return;
        }
        let sr = self.sample_rate as f32;
        let (low, chunk, max) = ((TAKE_LOW_SECONDS * sr) as usize, (TAKE_CHUNK_SECONDS * sr) as usize, (TAKE_SECONDS * sr) as usize);
        for k in 0..self.rec.in_engine.len() {
            let (track, source) = self.rec.in_engine[k];
            let Some(room) = self.engine.record_room(track) else { continue };
            let Some(g) = self.rec.granted.iter().position(|g| g.0 == track) else { continue };
            let (_, granted, sent) = self.rec.granted[g];
            // Le morceau précédent n'est peut-être pas encore arrivé au moteur.
            if room >= low || granted >= max || self.time - sent < 0.25 {
                continue;
            }
            let frames = chunk.min(max - granted);
            let piece = Box::new(ponzik_engine::RecordChunk::new(frames, source.is_mono()));
            self.rec.granted[g] = (track, granted + frames, self.time);
            self.send(Command::RecordChunk { track, chunk: piece });
        }
    }

    /// Prises revenues du moteur : fichier WAV, clips alignés, tampon gardé pour la suite.
    fn collect_takes(&mut self) {
        for take in self.engine.take_recordings() {
            if let Some(i) = self.rec.in_engine.iter().position(|&(t, s)| t == take.track && s == take.source) {
                self.rec.in_engine.remove(i);
            }
            self.rec.granted.retain(|g| g.0 != take.track);
            let mut take = take;
            // Les morceaux ajoutés en route sont recollés : la prise est d'un seul tenant.
            take.flatten();
            if take.len > 0 {
                if take.is_full() && take.capacity() as f32 >= TAKE_SECONDS * take.sample_rate * 0.99 {
                    self.toast(tr("Take reached its maximum length (10 min)"));
                }
                self.place_take(&take);
            }
            if self.rec.spare.len() < SPARE_TAKES {
                take.shrink_for_reuse((TAKE_START_SECONDS * take.sample_rate) as usize);
                self.rec.spare.push(take);
            }
        }
    }

    /// Retard total d'une prise, en trames : compensation du mixer, aller-retour
    /// du pilote et décalage manuel (ou calibré).
    fn take_shift(&self, rec: &RecordBuffer) -> usize {
        let offset = (self.rec.settings.offset_ms as f64 / 1000.0 * rec.sample_rate as f64).round() as i64;
        (rec.mix_latency as i64 + self.rec.latency.round_trip() as i64 + offset).max(0) as usize
    }

    /// Écrit la prise et la pose dans la playlist : un clip par tour de boucle,
    /// sur des lignes neuves ; le dernier tour complet joue, les autres sont
    /// gardés muets dans le même groupe de prises.
    pub(super) fn place_take(&mut self, rec: &RecordBuffer) {
        let sr = rec.sample_rate as f64;
        let spt = rec.samples_per_tick.max(1e-6);
        let shift = self.take_shift(rec);
        let punch = if self.rec.settings.punch { self.project.loop_range.map(|(a, b)| (a as f64, b as f64)) } else { None };
        let slices = take_slices(rec.len, rec.start_tick, &rec.jumps, shift, spt, punch, (0.05 * sr) as usize);
        if slices.is_empty() {
            self.toast(tr("Nothing was recorded inside the punch range"));
            return;
        }
        let track = rec.track.min(self.project.tracks.len() - 1);
        let track_name = self.project.tracks[track].name.clone();
        let dir = if self.rec.demo { std::env::temp_dir().join("ponzik-takes") } else { compat::projects_dir().join("Recordings") };
        let _ = std::fs::create_dir_all(&dir);
        let (name, path) = loop {
            let n = self.rec.counts.entry(track_name.clone()).or_insert(0);
            *n += 1;
            let name = tr_fmt!("{} take {}", track_name, *n);
            let path = dir.join(format!("{}.wav", file_safe(&name)));
            if !path.exists() || self.rec.demo {
                break (name, path);
            }
        };
        let len = rec.len;
        let left = rec.left[..len].to_vec();
        let mono = rec.is_mono();
        let depth = if self.rec.settings.float { BitDepth::Float32 } else { BitDepth::Pcm24 };
        let written = if mono {
            ponzik_project::write_wav_channels(&path, &left, 1, sr as u32, depth)
        } else {
            let audio: Vec<f32> = left.iter().zip(&rec.right[..len]).flat_map(|(&l, &r)| [l, r]).collect();
            ponzik_project::write_wav_channels(&path, &audio, 2, sr as u32, depth)
        };
        if let Err(e) = written {
            self.toast(&format!("{}: {e}", path.display()));
            return;
        }
        let right = if mono { left.clone() } else { rec.right[..len].to_vec() };
        let key = path.to_string_lossy().into_owned();
        self.samples.insert(key.clone(), Arc::new(Sample { name: name.clone(), sample_rate: rec.sample_rate, left, right, markers: Vec::new() }));
        self.waveforms.remove(&key);

        let base = self.used_lanes();
        let longest = slices.iter().map(|s| s.to - s.from).max().unwrap_or(0);
        let active = slices.iter().rposition(|s| (s.to - s.from) * 2 >= longest).unwrap_or(0);
        let clips: Vec<AudioClipDef> = slices
            .iter()
            .map(|s| {
                let (start, offset) = clip_placement(s, spt, sr);
                let end = s.tick + (s.to - s.from) as f64 / spt;
                AudioClipDef {
                    path: key.clone(),
                    lane: base.min(63),
                    track,
                    start,
                    len: (end - start as f64).round().max(1.0) as u32,
                    offset,
                    gain: 1.0,
                    fade_in: 0.003,
                    fade_out: 0.003,
                    ..Default::default()
                }
            })
            .collect();
        if clips.len() > 1 {
            // Loop recording: one take per pass, the last full one in the comp.
            let takes = clips.into_iter().enumerate().map(|(k, clip)| ponzik_project::Take { name: tr_fmt!("Take {}", k + 1), clip }).collect();
            let mut group = ponzik_project::TakeGroup::new(base.min(63), track, takes);
            group.choose_whole(active);
            self.project.take_groups.push(group);
            self.comp.selected = Some(self.project.take_groups.len() - 1);
        } else {
            self.project.audio_clips.extend(clips);
        }
        self.commit(tr("Record audio"));
        let secs = len as f64 / sr;
        let ms = Latency::ms(shift as u32, sr as u32);
        if slices.len() > 1 {
            self.toast(&tr_fmt!("{} takes recorded ({}), last full pass in the comp · swipe the take lanes to comp", slices.len(), name));
        } else {
            self.toast(&tr_fmt!("Take recorded: {} ({:.1} s, aligned by {:.1} ms)", name, secs, ms));
        }
    }

    /// Choisit une prise (Alt+clic) : elle joue, les autres prises de son groupe
    /// qui la chevauchent se taisent. Hors groupe, le clic coupe ou rallume le clip.
    pub fn choose_take(&mut self, clip: usize) {
        let Some(c) = self.project.audio_clips.get(clip).cloned() else { return };
        if c.take == 0 {
            self.project.audio_clips[clip].muted = !c.muted;
        } else {
            let (a, b) = (c.start, c.start + c.len);
            for (i, o) in self.project.audio_clips.iter_mut().enumerate() {
                if o.take == c.take && o.start < b && o.start + o.len > a {
                    o.muted = i != clip;
                }
            }
        }
        self.commit(tr("Choose take"));
    }

    /// Lance le calibrage : des clics sur la sortie, enregistrés par l'entrée.
    pub fn start_calibration(&mut self) {
        if self.rec.input.is_none() && !self.open_input() {
            return;
        }
        if self.playing {
            self.stop();
        }
        let buffer = RecordBuffer::with_duration(CALIBRATION_SECONDS, self.sample_rate as f32);
        self.send(Command::Calibrate(Some(Box::new(buffer))));
        self.rec.calibrating = Some(self.time);
        self.toast(tr("Calibrating: loop the output back to the input (cable, or mic close to the speaker)"));
    }

    fn collect_calibration(&mut self) {
        let Some(cal) = self.engine.take_calibration() else {
            if self.rec.calibrating.is_some_and(|t| self.time - t > CALIBRATION_SECONDS + 3.0) {
                // Rien n'est revenu (pas d'entrée qui tourne) : on abandonne.
                self.rec.calibrating = None;
                self.send(Command::Calibrate(None));
                self.toast(tr("Calibration failed: no input signal"));
            }
            return;
        };
        self.rec.calibrating = None;
        let sr = cal.sample_rate;
        let energy = |x: &[f32]| x.iter().map(|s| s * s).sum::<f32>();
        let channel = if cal.is_mono() || energy(&cal.left[..cal.len]) >= energy(&cal.right[..cal.len]) {
            &cal.left[..cal.len]
        } else {
            &cal.right[..cal.len]
        };
        match measure_round_trip(channel, sr) {
            Some(measured) => {
                let reported = self.rec.latency.round_trip();
                let offset = (measured as f32 - reported as f32) * 1000.0 / sr;
                self.rec.calibration = Some(Calibration { measured: measured as u32, reported });
                let mut s = self.rec.settings.clone();
                s.offset_ms = (offset * 10.0).round() / 10.0;
                self.set_record_settings(s);
                self.toast(&tr_fmt!(
                    "Measured round trip: {:.1} ms · offset set to {:+.1} ms",
                    measured as f32 * 1000.0 / sr,
                    offset
                ));
            }
            None => self.toast(tr("No click came back: check the loopback and the input level")),
        }
    }

    /// Vrai si le panneau d'enregistrement est sous la souris (il la capte).
    pub(super) fn record_covers(&self, input: &Input) -> bool {
        self.rec.panel && input.inside && self.rec.panel_rect.is_some_and(|r| r.contains(input.x, input.y))
    }

    /// Bouton bascule en pastille, avec sa LED.
    pub(super) fn chip(&mut self, c: &mut Canvas, input: &Input, id: u64, r: Rect, label: &str, on: bool, tip: &str) -> bool {
        let t = self.theme;
        let hov = self.hovered(input, id, r);
        let hv = self.hover_anim(id, hov);
        let onv = self.anim.to(id ^ 0x4f4e, on as u8 as f32, T_HOVER);
        let radius = r.h * 0.5;
        c.fill_rrect(r, radius, t.raised.mix(t.raised_hi, hv * 0.6).mix(t.accent_deep, onv * 0.45));
        c.stroke_rrect(r, radius, 1.0, t.hairline.mix(t.accent.alpha(0.5), onv));
        let (_, cy) = r.center();
        if onv > 0.01 {
            c.fill_circle(r.x + 13.0, cy, 6.0, t.accent.alpha(0.18 * onv));
        }
        c.fill_circle(r.x + 13.0, cy, 3.0, t.muted.mix(t.accent, onv));
        let (size, label) = self.fit_shrink(self.f.sans_med, 12.0, label, r.w - 28.0);
        self.text(c, self.f.sans_med, size, r.x + 23.0, cy, &label, t.text2.mix(t.text, onv.max(hv * 0.5)));
        if hov {
            self.set_tip(tip, "");
        }
        hov && input.pressed
    }

    /// Panneau d'enregistrement : entrée, VU-mètre, gain, écoute, latence, format.
    pub(super) fn record_panel(&mut self, c: &mut Canvas, input: &Input, screen: Rect) {
        if !self.rec.panel {
            self.rec.panel_rect = None;
            return;
        }
        let t = self.theme;
        let (ax, ay) = self.rec.panel_anchor;
        let x = (ax - PANEL_W * 0.5).clamp(8.0, (screen.w - PANEL_W - 8.0).max(8.0));
        let y = ay.clamp(TOP_BAR, (screen.h - PANEL_H - 8.0).max(TOP_BAR));
        let r = Rect::new(x, y, PANEL_W, PANEL_H);
        self.rec.panel_rect = Some(r);
        let appear = self.anim.to(wid("rec-panel", 0, 0), 1.0, T_LAYOUT);
        // Ombre et carte.
        for k in 1..=4 {
            let k = k as f32;
            c.fill_rrect(r.inset(-2.0 * k).translate(0.0, 2.0 * k), 16.0 + 2.0 * k, t.shadow.alpha(0.07 * appear * t.shadow_strength));
        }
        c.fill_rrect_vgrad(r, 16.0, t.panel.mix(t.raised, 0.35), t.panel);
        c.stroke_rrect(r, 16.0, 1.0, t.hairline.mix(t.accent.alpha(0.25), 0.4));
        let inner = r.inset(16.0);

        // En-tête : témoin d'enregistrement, titre, état, fermeture.
        let hy = inner.y + 14.0;
        let recording = self.record && self.playing && !self.rec.in_engine.is_empty();
        let pulse = if recording { 0.6 + 0.4 * (self.time * 5.0).sin().abs() } else { 1.0 };
        let armed_col = if self.rec.armed.is_empty() { t.muted } else { t.negative };
        if !self.rec.armed.is_empty() {
            c.fill_circle(inner.x + 8.0, hy, 11.0, t.negative.alpha(0.15 * pulse));
        }
        c.fill_circle(inner.x + 8.0, hy, 6.0, armed_col.alpha(pulse));
        self.text(c, self.f.sans_med, 16.0, inner.x + 24.0, hy, tr("Recording"), t.text);
        let status = match (recording, self.rec.armed.len()) {
            (true, 1) => tr_fmt!("Taking {}", self.project.tracks[self.rec.armed[0].0].name),
            (true, n) => tr_fmt!("Taking {} tracks", n),
            (false, 0) => tr("No track armed").to_string(),
            (false, 1) => tr_fmt!("{} armed", self.project.tracks[self.rec.armed[0].0].name),
            (false, n) => tr_fmt!("{} tracks armed", n),
        };
        let close = Rect::new(inner.right() - 28.0, hy - 14.0, 28.0, 28.0);
        self.text_right(c, self.f.sans, 12.0, close.x - 8.0, hy, &status, if recording { t.negative } else { t.text2 });
        if self.icon_button(c, input, wid("rec-close", 0, 0), close, Icon::Close, false, (tr("Close"), "")) {
            self.rec.panel = false;
        }

        // Entrée : périphérique et source.
        let mut y = inner.y + 40.0;
        self.text(c, self.f.sans, 11.0, inner.x, y + 6.0, tr("Input"), t.muted);
        y += 16.0;
        let dev_r = Rect::new(inner.x, y, inner.w, 34.0);
        let did = wid("rec-device", 0, 0);
        let dhov = self.hovered(input, did, dev_r);
        let dhv = self.hover_anim(did, dhov);
        c.fill_rrect(dev_r, 10.0, t.bg_deep.mix(t.raised, dhv * 0.6));
        c.stroke_rrect(dev_r, 10.0, 1.0, t.hairline);
        let current = match &self.rec.settings.device {
            Some(id) => self.rec.devices.iter().find(|d| &d.id == id).map_or(id.clone(), |d| d.name.clone()),
            None => tr("System default input").to_string(),
        };
        let (_, dcy) = dev_r.center();
        self.text_fit(c, self.f.sans_med, 13.0, dev_r.x + 12.0, dcy, &current, dev_r.w - 44.0, t.text);
        draw_icon(c, if self.rec.device_list { Icon::ChevronDown } else { Icon::ChevronRight }, dev_r.right() - 18.0, dcy, 14.0, t.text2);
        if dhov {
            let detail = self.rec.input.as_ref().map_or(tr("Input closed").to_string(), |(n, ch)| tr_fmt_n!("{}\n{} channel", "{}\n{} channels", *ch, n.as_str(), *ch));
            self.set_tip(&detail, "");
        }
        if dhov && input.pressed {
            self.rec.device_list = !self.rec.device_list;
            if self.rec.device_list && !self.rec.demo {
                self.rec.devices = ponzik_audio::input_devices();
            }
        }
        y += 42.0;
        if let Some(err) = self.rec.input_error.clone() {
            self.text_fit(c, self.f.sans_med, 12.0, inner.x, y + 8.0, &err, inner.w, t.negative);
            self.text_fit(c, self.f.sans, 11.0, inner.x, y + 24.0, tr("Pick another microphone below"), inner.w, t.text2);
            y += 36.0;
        }
        if self.rec.device_list {
            self.device_list(c, input, Rect::new(inner.x, y, inner.w, inner.bottom() - y));
            return;
        }

        let sources = [tr("Stereo"), tr("In 1 (mono)"), tr("In 2 (mono)")];
        let tips: [(&str, &str); 3] = [
            (tr("Both input channels, stereo take"), ""),
            (tr("Input 1 only, mono take (vocal mic)"), ""),
            (tr("Input 2 only, mono take"), ""),
        ];
        let sel = match self.rec.settings.source {
            InputSource::Stereo => 0,
            InputSource::Left => 1,
            InputSource::Right => 2,
        };
        if let Some(i) = self.segmented_tips(c, input, wid("rec-source", 0, 0), Rect::new(inner.x, y, inner.w, 32.0), &sources, &tips, sel) {
            let mut s = self.rec.settings.clone();
            s.source = [InputSource::Stereo, InputSource::Left, InputSource::Right][i];
            self.set_record_settings(s);
        }
        y += 44.0;

        // VU-mètre d'entrée.
        y = self.input_meter(c, input, Rect::new(inner.x, y, inner.w, 92.0));

        // Gain et traitements d'entrée.
        let gain = self.rec.settings.strip.gain_db;
        let gain_r = Rect::new(inner.x, y, 132.0, 40.0);
        let text = ponzik_i18n::localize_number(format!("{gain:+.1}"));
        if let Some(v) = self.drag_field(c, input, wid("rec-gain", 0, 0), gain_r, gain, 0.1, 0.5, &text, "dB") {
            let mut s = self.rec.settings.clone();
            s.strip.gain_db = (v * 2.0).round() / 2.0;
            s.strip.gain_db = s.strip.gain_db.clamp(-24.0, 24.0);
            self.set_record_settings(s);
        }
        if input.double && gain_r.contains(input.x, input.y) {
            let mut s = self.rec.settings.clone();
            s.strip.gain_db = 0.0;
            self.set_record_settings(s);
        }
        self.tip_zone(input, gain_r, tr("Input gain\nDrag or scroll · double-click: 0 dB\nAim for peaks around -12 dBFS"), "");
        let cw = (inner.w - 140.0 - 8.0) * 0.5;
        let s = self.rec.settings.clone();
        let low_cut = Rect::new(inner.x + 140.0, y, cw, 40.0);
        if self.chip(c, input, wid("rec-hpf", 0, 0), low_cut, tr("Low cut"), s.strip.high_pass, tr("80 Hz high-pass\nRemoves rumble and mic stand thumps")) {
            let mut n = s.clone();
            n.strip.high_pass = !n.strip.high_pass;
            self.set_record_settings(n);
        }
        let gate_r = Rect::new(low_cut.right() + 8.0, y, cw, 40.0);
        let gate_on = s.strip.gate;
        if self.chip(c, input, wid("rec-gate", 0, 0), gate_r, tr("Gate"), gate_on, tr("Gentle noise gate\nLowers the noise by 18 dB between phrases (never cuts dead)")) {
            let mut n = s.clone();
            n.strip.gate = !n.strip.gate;
            self.set_record_settings(n);
        }
        if gate_on {
            // Témoin d'ouverture de la porte.
            let open = self.rec.gate.clamp(0.0, 1.0);
            c.fill_circle(gate_r.right() - 14.0, gate_r.y + gate_r.h * 0.5, 4.0, t.muted.mix(t.accent, open));
        }
        y += 52.0;

        // Écoute.
        self.text(c, self.f.sans, 11.0, inner.x, y + 6.0, tr("Monitoring"), t.muted);
        y += 16.0;
        let half = (inner.w - 8.0) * 0.5;
        let s = self.rec.settings.clone();
        if self.chip(c, input, wid("rec-mon", 0, 0), Rect::new(inner.x, y, half, 34.0), tr("Hear input"), s.monitor, tr("Hear the armed tracks' input through their effects\nUse headphones to avoid feedback")) {
            let mut n = s.clone();
            n.monitor = !n.monitor;
            self.set_record_settings(n);
        }
        if self.chip(
            c,
            input,
            wid("rec-lowlat", 0, 0),
            Rect::new(inner.x + half + 8.0, y, half, 34.0),
            tr("Low latency"),
            s.low_latency,
            tr("Low-latency monitoring\nLookahead effects on the monitored path are bypassed\nand delay compensation is skipped. Takes stay dry and aligned."),
        ) {
            let mut n = s.clone();
            n.low_latency = !n.low_latency;
            self.set_record_settings(n);
        }
        y += 44.0;

        // Latence : ce qu'on entend, et le recalage des prises.
        y = self.latency_card(c, input, Rect::new(inner.x, y, inner.w, 100.0));

        // Format et punch.
        let s = self.rec.settings.clone();
        let fmt = [tr("24-bit"), tr("32-bit float")];
        let ftips: [(&str, &str); 2] = [(tr("24-bit PCM WAV at the device rate"), ""), (tr("32-bit float WAV at the device rate"), "")];
        let fr = Rect::new(inner.x, y, 152.0, 32.0);
        if let Some(i) = self.segmented_tips(c, input, wid("rec-format", 0, 0), fr, &fmt, &ftips, s.float as usize) {
            let mut n = s.clone();
            n.float = i == 1;
            self.set_record_settings(n);
        }
        let pw = (inner.w - fr.w - 8.0 - 6.0) * 0.5;
        let punch_r = Rect::new(fr.right() + 8.0, y, pw, 32.0);
        let punch_tip = if self.project.loop_range.is_some() {
            tr("Punch in/out\nKeep only what falls inside the time selection")
        } else {
            tr("Punch in/out\nDraw a time selection in the playlist ruler first")
        };
        if self.chip(c, input, wid("rec-punch", 0, 0), punch_r, tr("Punch"), s.punch, punch_tip) {
            let mut n = s.clone();
            n.punch = !n.punch;
            self.set_record_settings(n);
        }
        let ci_r = Rect::new(punch_r.right() + 6.0, y, pw, 32.0);
        if self.chip(c, input, wid("rec-countin", 0, 0), ci_r, tr("Count-in"), self.count_in, tr("One bar of metronome before recording")) {
            self.toggle_count_in();
        }
        y += 40.0;
        let hint = if self.project.loop_range.is_some() {
            tr("Loop recording: every pass of the selection becomes a take")
        } else {
            tr("Tip: select a range in the ruler to loop and stack takes")
        };
        self.text_fit(c, self.f.sans, 11.0, inner.x, y + 6.0, hint, inner.w, t.muted);
    }

    /// VU-mètre stéréo horizontal, graduations, crête maintenue et témoin de saturation.
    /// Renvoie le bas de la zone.
    fn input_meter(&mut self, c: &mut Canvas, input: &Input, r: Rect) -> f32 {
        let t = self.theme;
        c.fill_rrect(r, 12.0, t.bg_deep);
        c.stroke_rrect(r, 12.0, 1.0, t.hairline);
        let clip_r = Rect::new(r.right() - 58.0, r.y + 10.0, 46.0, 22.0);
        let bars = Rect::new(r.x + 12.0, r.y + 12.0, clip_r.x - r.x - 22.0, 18.0);
        let pos = |db: f32| ((db - METER_FLOOR) / -METER_FLOOR).clamp(0.0, 1.0);
        let to_db = |g: f32| 20.0 * g.max(1e-6).log10();
        let yellow = self.theme.warning;
        for ch in 0..2 {
            let bar = Rect::new(bars.x, bars.y + ch as f32 * (bars.h * 0.5 + 3.0), bars.w, bars.h * 0.5);
            c.fill_rrect(bar, 3.0, t.well);
            let lv = pos(to_db(self.rec.level[ch]));
            // Trois zones : vert jusqu'à −12, ambre jusqu'à −3, rouge au-dessus.
            let zones = [(METER_FLOOR, -12.0, t.accent), (-12.0, -3.0, yellow), (-3.0, 0.0, t.negative)];
            for (a, b, col) in zones {
                let (pa, pb) = (pos(a), pos(b).min(lv));
                if pb > pa {
                    c.fill_rect(Rect::new(bar.x + pa * bar.w, bar.y, (pb - pa) * bar.w, bar.h), col);
                }
            }
        }
        let hold = pos(to_db(self.rec.hold[0]));
        if hold > 0.0 {
            let col = if hold > pos(-3.0) { t.negative } else { t.text };
            c.fill_rect(Rect::new(bars.x + hold * bars.w - 1.0, bars.y - 2.0, 2.0, bars.h + 5.0), col);
        }
        for db in METER_TICKS {
            let x = bars.x + pos(db) * bars.w;
            c.fill_rect(Rect::new(x, bars.bottom() + 6.0, 1.0, 4.0), t.muted.alpha(0.6));
            let label = format!("{}", db as i32);
            self.text_center(c, self.f.mono, 9.0, x, bars.bottom() + 17.0, &label, t.muted);
        }
        // Zone idéale pour une voix : crêtes entre −18 et −6 dBFS.
        let (za, zb) = (bars.x + pos(-18.0) * bars.w, bars.x + pos(-6.0) * bars.w);
        c.fill_rrect(Rect::new(za, bars.bottom() + 24.0, zb - za, 3.0), 1.5, t.accent.alpha(0.35));
        self.text_center(c, self.f.sans, 9.0, (za + zb) * 0.5, bars.bottom() + 34.0, tr("sweet spot"), t.muted);
        // Crête en dBFS.
        let peak_db = to_db(self.rec.hold[0]);
        let readout = if peak_db <= METER_FLOOR { "-inf".to_string() } else { ponzik_i18n::localize_number(format!("{peak_db:.1}")) };
        self.text(c, self.f.mono_med, 13.0, r.x + 12.0, r.bottom() - 14.0, &readout, t.text);
        self.text(c, self.f.sans, 10.0, r.x + 12.0 + self.fonts.measure(self.f.mono_med, 13.0, &readout) + 4.0, r.bottom() - 14.0, tr("dBFS peak"), t.muted);
        // Témoin de saturation, verrouillé jusqu'au clic.
        let cid = wid("rec-clip", 0, 0);
        let chov = self.hovered(input, cid, clip_r);
        let lit = self.anim.to(cid ^ 1, self.rec.clipped as u8 as f32, T_PRESS);
        c.fill_rrect(clip_r, 6.0, t.raised.mix(t.negative, lit));
        if lit > 0.01 {
            c.fill_rrect(clip_r.inset(-3.0), 9.0, t.negative.alpha(0.2 * lit));
        }
        let (cx, cy) = clip_r.center();
        self.text_center(c, self.f.sans_med, 11.0, cx, cy, "CLIP", t.muted.mix(t.highlight, lit));
        if chov {
            self.set_tip(if self.rec.clipped { tr("The input clipped: lower the gain or the preamp\nClick to reset") } else { tr("Lights up when the input clips") }, "");
            if input.pressed {
                self.rec.clipped = false;
            }
        }
        r.bottom() + 12.0
    }

    /// Carte de latence : écoute (ce que le chanteur entend) et recalage des prises.
    fn latency_card(&mut self, c: &mut Canvas, input: &Input, r: Rect) -> f32 {
        let t = self.theme;
        let sr = self.sample_rate;
        c.fill_rrect(r, 12.0, t.bg_deep);
        c.stroke_rrect(r, 12.0, 1.0, t.hairline);
        let lat = self.rec.latency;
        let mix = self.engine.latency() as u32;
        // Écoute : aller-retour du pilote, plus la compensation si l'écoute ne la contourne pas.
        let hear = lat.round_trip() + if self.rec.settings.low_latency { 0 } else { mix };
        let hear_ms = Latency::ms(hear, sr);
        let open = self.rec.input.is_some() || self.rec.demo;
        let big = if open { tr_fmt!("{:.1} ms", hear_ms) } else { "--".to_string() };
        let col = if !open {
            t.muted
        } else if hear_ms <= 12.0 {
            t.accent
        } else if hear_ms <= 25.0 {
            self.theme.warning
        } else {
            t.negative
        };
        self.text(c, self.f.sans, 11.0, r.x + 12.0, r.y + 16.0, tr("You hear yourself after"), t.muted);
        self.text(c, self.f.mono_med, 24.0, r.x + 12.0, r.y + 40.0, &big, col);
        let verdict = if !open {
            tr("input closed")
        } else if hear_ms <= 12.0 {
            tr("imperceptible")
        } else if hear_ms <= 25.0 {
            tr("slight")
        } else {
            tr("noticeable: lower the buffer")
        };
        self.text(c, self.f.sans, 11.0, r.x + 12.0, r.y + 62.0, verdict, col.alpha(0.85));
        let detail = tr_fmt!(
            "out {:.1}  in {:.1}  mix {:.1}",
            Latency::ms(lat.output, sr),
            Latency::ms(lat.input, sr),
            Latency::ms(mix, sr)
        );
        let (size, detail) = self.fit_shrink(self.f.mono, 10.0, &detail, r.w * 0.52 - 16.0);
        self.text(c, self.f.mono, size, r.x + 12.0, r.y + 84.0, &detail, t.text2);
        self.tip_zone(
            input,
            Rect::new(r.x, r.y, r.w * 0.5, r.h),
            tr("Monitoring latency: output + input buffers, measured live\n(+ delay compensation when low latency is off)\nPONZIK_LATENCY_MS sets the buffer (default 10 ms)"),
            "",
        );

        // Recalage des prises : décalage manuel et calibrage.
        let x = r.x + r.w * 0.52;
        self.text(c, self.f.sans, 11.0, x, r.y + 16.0, tr("Take offset"), t.muted);
        let off_r = Rect::new(x, r.y + 26.0, r.right() - x - 12.0, 32.0);
        let off = self.rec.settings.offset_ms;
        let text = ponzik_i18n::localize_number(format!("{off:+.1}"));
        if let Some(v) = self.drag_field(c, input, wid("rec-offset", 0, 0), off_r, off, 0.1, 0.1, &text, "ms") {
            let mut s = self.rec.settings.clone();
            s.offset_ms = ((v * 10.0).round() / 10.0).clamp(-200.0, 200.0);
            self.set_record_settings(s);
        }
        self.tip_zone(input, off_r, tr("Extra delay compensation for takes, on top of the measured latency\nDrag or scroll · Calibrate to measure it"), "");
        let cal_r = Rect::new(x, r.y + 64.0, off_r.w, 26.0);
        let kid = wid("rec-cal", 0, 0);
        let khov = self.hovered(input, kid, cal_r);
        let khv = self.hover_anim(kid, khov);
        let busy = self.rec.calibrating.is_some();
        c.fill_rrect(cal_r, 8.0, t.raised.mix(t.raised_hi, khv).mix(t.accent_deep, busy as u8 as f32 * 0.5));
        let (cx, cy) = cal_r.center();
        let label = if busy { tr("Listening...") } else { tr("Calibrate") };
        self.text_center(c, self.f.sans_med, 12.0, cx, cy, label, t.text);
        if khov {
            let tip = match self.rec.calibration {
                Some(cal) => tr_fmt!(
                    "Plays clicks and records them back to measure the true round trip\nLast: measured {:.1} ms, reported {:.1} ms",
                    Latency::ms(cal.measured, sr),
                    Latency::ms(cal.reported, sr)
                ),
                None => tr("Plays clicks and records them back to measure the true round trip\nLoop the output into the input (cable or mic near the speaker)").to_string(),
            };
            self.set_tip(&tip, "");
        }
        if khov && input.pressed && !busy {
            self.start_calibration();
        }
        r.bottom() + 12.0
    }

    /// Liste des entrées, à la place du reste du panneau.
    fn device_list(&mut self, c: &mut Canvas, input: &Input, r: Rect) {
        let t = self.theme;
        let mut items: Vec<(Option<String>, String)> = vec![(None, tr("System default input").to_string())];
        items.extend(self.rec.devices.iter().filter(|d| d.id != "default").map(|d| (Some(d.id.clone()), d.name.clone())));
        let mut y = r.y;
        for (i, (id, name)) in items.iter().enumerate() {
            if y + 34.0 > r.bottom() {
                break;
            }
            let row = Rect::new(r.x, y, r.w, 34.0);
            let rid = wid("rec-dev", i, 0);
            let hov = self.hovered(input, rid, row);
            let hv = self.hover_anim(rid, hov);
            let sel = *id == self.rec.settings.device;
            c.fill_rrect(row, 8.0, t.raised.alpha(hv.max(sel as u8 as f32 * 0.7)));
            let (_, cy) = row.center();
            if sel {
                draw_icon(c, Icon::Check, row.x + 14.0, cy, 13.0, t.accent);
            }
            self.text_fit(c, self.f.sans, 12.0, row.x + 30.0, cy, name, row.w - 40.0, if sel { t.text } else { t.text2 });
            if hov {
                if let Some(id) = id {
                    self.set_tip(id, "");
                }
                if input.pressed {
                    let mut s = self.rec.settings.clone();
                    s.device = id.clone();
                    self.rec.device_list = false;
                    self.set_record_settings(s);
                }
            }
            y += 38.0;
        }
        if items.len() == 1 {
            self.text(c, self.f.sans, 11.0, r.x + 8.0, y + 10.0, tr("No other input found"), t.muted);
        }
    }

    /// Capture d'écran : une piste armée, le panneau ouvert, des niveaux simulés ;
    /// `takes` pose en plus un enregistrement de trois tours de boucle.
    pub fn screenshot_record(&mut self, mode: &str) {
        self.rec.demo = true;
        self.rec.devices = vec![
            DeviceInfo { id: "default".into(), name: "System default input".into() },
            DeviceInfo { id: "pipewire".into(), name: "PipeWire Sound Server".into() },
            DeviceInfo { id: "plughw:CARD=USB,DEV=0".into(), name: "Scarlett 2i2 USB · USB Audio".into() },
        ];
        self.rec.input = Some(("Scarlett 2i2 USB".into(), 2));
        self.rec.latency = Latency { output: 288, input: 192 };
        self.rec.clipped = std::env::var_os("PONZIK_CLIP").is_some();
        self.rec.settings.strip.high_pass = true;
        let track = 3;
        self.project.tracks[track].name = "Vocals".into();
        self.rec.armed.push((track, InputSource::Left));
        self.record = true;
        self.project.loop_range = Some((4 * ponzik_engine::TICKS_PER_BAR, 8 * ponzik_engine::TICKS_PER_BAR));
        self.time_selection = Some((super::app::View::Playlist, 4 * ponzik_engine::TICKS_PER_BAR, 8 * ponzik_engine::TICKS_PER_BAR));
        if mode == "takes" {
            // Trois tours de la sélection (4 mesures à 120 BPM : 8 s), une voix synthétique.
            let sr = self.sample_rate as f32;
            let spt = sr as f64 * 60.0 / (self.project.bpm as f64 * ponzik_engine::PPQ as f64);
            let pass = (4.0 * ponzik_engine::TICKS_PER_BAR as f64 * spt) as usize;
            let mut rec = RecordBuffer::for_source(InputSource::Left, 30.0, sr);
            let total = pass * 3 + pass / 2;
            for k in 0..total {
                let ts = k as f32 / sr;
                let phrase = ((ts * 0.8).sin() * 0.5 + 0.5) * ((ts * 3.1).sin().abs());
                rec.left[k] = phrase * 0.4 * (ts * 220.0 * std::f32::consts::TAU).sin();
            }
            rec.len = total;
            rec.track = track;
            rec.samples_per_tick = spt;
            rec.start_tick = 4.0 * ponzik_engine::TICKS_PER_BAR as f64;
            rec.jumps = (1..4).map(|k| (k * pass, 4.0 * ponzik_engine::TICKS_PER_BAR as f64)).collect();
            self.place_take(&rec);
        }
        self.open_record_panel(Some((1040.0, TOP_BAR + 4.0)));
        self.rec.device_list = mode == "devices";
    }
}

/// Nom de fichier sans caractères gênants.
pub(super) fn file_safe(name: &str) -> String {
    name.chars().map(|c| if c.is_alphanumeric() || " -_".contains(c) { c } else { '_' }).collect::<String>().trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_and_tolerate_garbage() {
        let s = RecordSettings {
            device: Some("plughw:CARD=USB,DEV=0".into()),
            source: InputSource::Right,
            strip: InputStrip { gain_db: 4.5, high_pass: true, gate: true, gate_threshold_db: -42.0 },
            monitor: false,
            low_latency: false,
            offset_ms: -3.2,
            float: true,
            punch: false,
        };
        assert_eq!(RecordSettings::from_text(&s.to_text()), s);
        let bad = RecordSettings::from_text("gain_db=NaN\noffset_ms=9999\nsource=???\nnot a line\n");
        assert_eq!((bad.strip.gain_db, bad.offset_ms, bad.source), (0.0, 200.0, InputSource::Left));
    }

    #[test]
    fn file_names_are_safe() {
        assert_eq!(file_safe("Lead / Vox: take 2"), "Lead _ Vox_ take 2");
    }
}
