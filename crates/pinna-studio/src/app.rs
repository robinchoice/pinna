//! The window: transport and A/B on top, the arrangement with ghosts in the middle,
//! the proposal column on the right.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Key, Rect, RichText, Sense, Shape, Stroke,
    StrokeKind, pos2, vec2,
};
use pinna_core::{Clip, MASTER, PPQ, Project};
use pinna_engine::RenderOptions;
use pinna_engine::live::Song;
use serde::{Deserialize, Serialize};

use crate::agent::{self, Job};
use crate::audio::{self, Audio};
use crate::state::{Loaded, Prop, Watcher};
use crate::style::{self, *};
use crate::text::{Better, Lang, metric};

const HEAD_W: f32 = 130.0;
const RULER_H: f32 = 24.0;
const ROW_H: f32 = 62.0;
const MASTER_H: f32 = 40.0;
const SIDE_W: f32 = 360.0;
/// Samples of played audio the live spectrum looks at.
const SCOPE: usize = 8192;

#[derive(Default, Serialize, Deserialize)]
struct Saved {
    lang: Option<Lang>,
    last: Option<PathBuf>,
}

enum Action {
    Seek(u64),
    Loop(String),
    Select(String),
    /// Select from a ghost: also bring the proposal into view.
    Reveal(String),
    Hear(bool),
    Accept(String),
    Discard(String),
    Revise(String, String),
    Task(String),
    Cancel(u64),
    Dismiss(u64),
}

pub struct App {
    lang: Lang,
    audio: Result<Audio, String>,
    watcher: Option<Watcher>,
    loaded: Option<Loaded>,
    generation: u64,
    error: Option<String>,
    /// Name of the open proposal shown and heard as B.
    selected: Option<String>,
    hear_b: bool,
    /// Gain matching B to A, per proposal hash.
    gains: HashMap<String, f64>,
    gain_pending: Option<String>,
    gain_tx: Sender<(String, f64)>,
    gain_rx: Receiver<(String, f64)>,
    song_key: (u64, Option<String>),
    /// Where playback starts, in samples.
    cursor: u64,
    loop_section: Option<String>,
    loop_set: Option<(u64, u64)>,
    jobs: Vec<(u64, Job)>,
    next_job: u64,
    task: String,
    revise: String,
    /// Scroll the column to the selected proposal (after a click on its ghost).
    reveal: bool,
    toast: Option<(String, bool, Instant)>,
    busy: Option<Receiver<Result<(String, bool), String>>>,
    scope: Vec<f32>,
    bands: Vec<f64>,
    bands_at: Instant,
    open_path: String,
    #[cfg(feature = "screenshot")]
    shot: crate::screenshot::Shot,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, arg: Option<PathBuf>) -> Self {
        style::apply(&cc.egui_ctx);
        let saved: Saved = cc
            .storage
            .and_then(|s| eframe::get_value(s, eframe::APP_KEY))
            .unwrap_or_default();
        let (gain_tx, gain_rx) = mpsc::channel();
        let mut app = Self {
            lang: saved.lang.unwrap_or_else(Lang::from_env),
            audio: audio::start(),
            watcher: None,
            loaded: None,
            generation: 0,
            error: None,
            selected: None,
            hear_b: false,
            gains: HashMap::new(),
            gain_pending: None,
            gain_tx,
            gain_rx,
            song_key: (0, None),
            cursor: 0,
            loop_section: None,
            loop_set: None,
            jobs: Vec::new(),
            next_job: 1,
            task: String::new(),
            revise: String::new(),
            reveal: false,
            toast: None,
            busy: None,
            scope: Vec::new(),
            bands: Vec::new(),
            bands_at: Instant::now(),
            open_path: String::new(),
            #[cfg(feature = "screenshot")]
            shot: crate::screenshot::Shot::from_env(),
        };
        let start = arg.or(saved.last.filter(|p| p.join("pinna.ron").exists()));
        if let Some(root) = start {
            app.open(&cc.egui_ctx, &root);
        }
        app
    }

    fn open(&mut self, ctx: &egui::Context, root: &Path) {
        let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        if let Ok(a) = &self.audio {
            a.player.stop();
        }
        let ctx = ctx.clone();
        self.watcher = Some(Watcher::start(root.clone(), move || ctx.request_repaint()));
        self.loaded = None;
        self.error = None;
        self.selected = None;
        self.hear_b = false;
        self.cursor = 0;
        self.loop_section = None;
        self.open_path = root.display().to_string();
    }

    fn sample_rate(&self) -> u32 {
        self.audio
            .as_ref()
            .map_or(pinna_engine::DEFAULT_SAMPLE_RATE, |a| {
                a.player.sample_rate()
            })
    }

    fn playing(&self) -> bool {
        self.audio.as_ref().is_ok_and(|a| a.player.is_playing())
    }

    /// Playback position, or where it will start, in samples.
    fn position(&self) -> u64 {
        match &self.audio {
            Ok(a) if a.player.is_playing() => a.player.position(),
            _ => self.cursor,
        }
    }

    fn toast(&mut self, text: impl Into<String>, ok: bool) {
        self.toast = Some((text.into(), ok, Instant::now()));
    }

    fn selected_prop(&self) -> Option<&Prop> {
        let name = self.selected.as_deref()?;
        self.loaded
            .as_ref()?
            .open
            .iter()
            .find(|p| p.info.name == name)
    }

    /// Take in what the background threads produced.
    fn poll(&mut self) {
        while let Some(msg) = self.watcher.as_ref().and_then(|w| w.rx.try_recv().ok()) {
            match msg {
                Ok(l) => {
                    let keep = self
                        .selected
                        .as_ref()
                        .is_some_and(|s| l.open.iter().any(|p| &p.info.name == s));
                    if !keep {
                        self.selected = l.open.first().map(|p| p.info.name.clone());
                        self.hear_b = false;
                    }
                    self.loaded = Some(l);
                    self.generation += 1;
                    self.error = None;
                }
                Err(e) => self.error = Some(e),
            }
        }
        while let Ok((hash, gain)) = self.gain_rx.try_recv() {
            self.gains.insert(hash, gain);
            self.gain_pending = None;
        }
        if let Some(rx) = &self.busy
            && let Ok(result) = rx.try_recv()
        {
            self.busy = None;
            match result {
                Ok((msg, accepted)) => {
                    if accepted {
                        self.hear_b = false;
                    }
                    self.selected = None;
                    self.toast(msg, true);
                }
                Err(e) => self.toast(e, false),
            }
            if let Some(w) = &self.watcher {
                w.poke();
            }
        }
        let before = self.jobs.len();
        self.jobs.retain(|(_, j)| {
            let s = j.snapshot();
            !(s.proposed && matches!(s.done, Some(Ok(_))))
        });
        if self.jobs.len() != before
            && let Some(w) = &self.watcher
        {
            w.poke();
        }
        if let Some(e) = self.audio.as_ref().ok().and_then(|a| a.player.take_error()) {
            self.toast(e, false);
        }
    }

    /// Keep the player on the current song, proposal, loop and A/B choice.
    fn sync(&mut self) {
        let Ok(audio) = &self.audio else {
            return;
        };
        let Some(l) = &self.loaded else {
            return;
        };
        let prop = self
            .selected
            .as_deref()
            .and_then(|n| l.open.iter().find(|p| p.info.name == n));
        let key = (self.generation, prop.map(|p| p.info.hash.clone()));
        if key != self.song_key {
            audio.player.set_song(Song {
                a: l.project.clone(),
                b: prop.and_then(|p| p.preview.clone()),
                root: l.root.clone(),
            });
            self.song_key = key;
        }
        let gain = prop.and_then(|p| self.gains.get(&p.info.hash));
        audio.player.set_b(self.hear_b && prop.is_some());
        audio.player.set_b_gain_db(gain.copied().unwrap_or(0.0));
        let sr = audio.player.sample_rate();
        let t = l.project.timing();
        let want = self.loop_section.as_ref().and_then(|name| {
            let s = l.project.section(name)?;
            Some((t.sample(t.tick(s.from), sr), t.sample(t.tick(s.to), sr)))
        });
        if want != self.loop_set {
            audio.player.set_loop(want);
            self.loop_set = want;
        }
        if let Some(p) = prop
            && gain.is_none()
            && self.gain_pending.as_ref() != Some(&p.info.hash)
            && let Some(b) = p.preview.clone()
        {
            let (a, root, range, hash) = (
                l.project.clone(),
                l.root.clone(),
                p.range,
                p.info.hash.clone(),
            );
            self.gain_pending = Some(hash.clone());
            let tx = self.gain_tx.clone();
            std::thread::spawn(move || {
                let _ = tx.send((hash, match_gain(&a, &b, &root, range)));
            });
        }
    }

    fn spectrum(&mut self) {
        let Ok(audio) = &mut self.audio else {
            return;
        };
        audio.player.drain_scope(&mut self.scope);
        if self.scope.len() > SCOPE {
            self.scope.drain(..self.scope.len() - SCOPE);
        }
        if self.bands_at.elapsed() > Duration::from_millis(100) {
            self.bands_at = Instant::now();
            self.bands = if self.scope.iter().any(|x| *x != 0.0) {
                live_bands(&self.scope, audio.player.sample_rate())
            } else {
                Vec::new()
            };
        }
    }

    fn toggle_play(&mut self) {
        let Ok(audio) = &self.audio else {
            return;
        };
        if audio.player.is_playing() {
            self.cursor = audio.player.position();
            audio.player.stop();
            return;
        }
        if let Some(l) = &self.loaded {
            let t = l.project.timing();
            if self.cursor >= t.sample(l.project.end_tick(), audio.player.sample_rate()) {
                self.cursor = 0;
            }
        }
        audio.player.play(self.cursor);
    }

    fn apply(&mut self, ctx: &egui::Context, actions: Vec<Action>) {
        for a in actions {
            match a {
                Action::Seek(s) => {
                    self.cursor = s;
                    if let Ok(audio) = &self.audio
                        && audio.player.is_playing()
                    {
                        audio.player.play(s);
                    }
                }
                Action::Loop(name) => {
                    if self.loop_section.as_ref() == Some(&name) {
                        self.loop_section = None;
                    } else {
                        if let Some(l) = &self.loaded
                            && let Some(s) = l.project.section(&name)
                        {
                            let t = l.project.timing();
                            self.cursor = t.sample(t.tick(s.from), self.sample_rate());
                        }
                        self.loop_section = Some(name);
                        if let Ok(audio) = &self.audio
                            && audio.player.is_playing()
                        {
                            audio.player.play(self.cursor);
                        }
                    }
                }
                Action::Reveal(name) => {
                    self.reveal = true;
                    if self.selected.as_ref() != Some(&name) {
                        self.selected = Some(name);
                        self.revise.clear();
                    }
                }
                Action::Select(name) => {
                    if self.selected.as_ref() != Some(&name) {
                        self.selected = Some(name);
                        self.revise.clear();
                    }
                }
                Action::Hear(b) => self.hear_b = b,
                Action::Accept(name) => self.decide(name, true),
                Action::Discard(name) => self.decide(name, false),
                Action::Revise(name, text) => {
                    let task = format!("{text} (revise the proposal \"{name}\")");
                    self.start_job(ctx, &task, &format!("proposal/{name}"));
                }
                Action::Task(text) => self.start_job(ctx, &text, "HEAD"),
                Action::Cancel(id) => {
                    if let Some((_, j)) = self.jobs.iter().find(|(i, _)| *i == id) {
                        j.cancel();
                    }
                }
                Action::Dismiss(id) => self.jobs.retain(|(i, _)| *i != id),
            }
        }
    }

    fn decide(&mut self, name: String, accept: bool) {
        let Some(l) = &self.loaded else {
            return;
        };
        if self.busy.is_some() {
            return;
        }
        let root = l.root.clone();
        let (tx, rx) = mpsc::channel();
        self.busy = Some(rx);
        std::thread::spawn(move || {
            let result = if accept {
                pinna::proposal::accept(&root, &name)
            } else {
                pinna::proposal::discard(&root, &name)
            };
            let _ = tx.send(result.map(|m| (m.trim().to_string(), accept)));
        });
    }

    fn start_job(&mut self, ctx: &egui::Context, task: &str, base: &str) {
        let Some(l) = &self.loaded else {
            return;
        };
        match agent::start(&l.root, self.next_job, task, base, self.lang.name()) {
            Ok(job) => {
                self.jobs.insert(0, (self.next_job, job));
                self.next_job += 1;
            }
            Err(e) => self.toast(e, false),
        }
        ctx.request_repaint();
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll();
        self.sync();
        self.spectrum();
        let typing = ctx.memory(|m| m.focused().is_some());
        if !typing && ctx.input(|i| i.key_pressed(Key::Space)) && self.loaded.is_some() {
            self.toggle_play();
        }
        let mut actions = Vec::new();
        if self.loaded.is_none() {
            self.start_screen(ui);
        } else {
            self.top_bar(ui, &mut actions);
            self.side(ui, &mut actions);
            self.arrangement(ui, &mut actions);
        }
        self.toasts(&ctx);
        // The column scrolled to it this frame; a new reveal comes from the actions.
        self.reveal = false;
        self.apply(&ctx, actions);
        #[cfg(feature = "screenshot")]
        {
            let settled = self.loaded.is_some()
                && self.gain_pending.is_none()
                && self
                    .selected_prop()
                    .is_none_or(|p| self.gains.contains_key(&p.info.hash));
            if self.shot.frame(&ctx, settled, &mut self.hear_b) {
                self.toggle_play();
            }
        }
        if self.playing() || self.busy.is_some() {
            ctx.request_repaint();
        } else {
            ctx.request_repaint_after(Duration::from_millis(300));
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        let saved = Saved {
            lang: Some(self.lang),
            last: self.loaded.as_ref().map(|l| l.root.clone()),
        };
        eframe::set_value(storage, eframe::APP_KEY, &saved);
    }
}

// ---- drawing ----

impl App {
    fn start_screen(&mut self, ui: &mut egui::Ui) {
        let lang = self.lang;
        let mut open = None;
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(BG))
            .show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(ui.available_height() * 0.25);
                    ui.label(RichText::new("PINNA").size(28.0).strong().color(ACCENT));
                    ui.add_space(4.0);
                    if self.watcher.is_some() && self.error.is_none() {
                        ui.label(
                            RichText::new(lang.t("Lade Projekt …", "Loading project …"))
                                .color(MUTED),
                        );
                        return;
                    }
                    ui.label(RichText::new(lang.t(
                        "Öffne einen Projektordner mit pinna.ron oder starte mit dem Beispielsong.",
                        "Open a project folder with pinna.ron or start with the example song.",
                    )).color(MUTED));
                    ui.add_space(16.0);
                    if ui
                        .add(primary(
                            lang.t("Beispiel „Nachtzug“ öffnen", "Open example “Nachtzug”"),
                        ))
                        .clicked()
                    {
                        open = Some(example_dir());
                    }
                    ui.add_space(6.0);
                    if ui
                        .button(lang.t("Projektordner öffnen …", "Open project folder …"))
                        .clicked()
                        && let Some(dir) = rfd::FileDialog::new().pick_folder()
                    {
                        open = Some(Ok(dir));
                    }
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        let w = 420.0;
                        ui.add_space((ui.available_width() - w - 80.0).max(0.0) / 2.0);
                        ui.add(
                            egui::TextEdit::singleline(&mut self.open_path)
                                .hint_text(
                                    lang.t("Pfad zum Projektordner", "Path to the project folder"),
                                )
                                .desired_width(w),
                        );
                        if ui.button(lang.t("Öffnen", "Open")).clicked() {
                            open = Some(Ok(PathBuf::from(self.open_path.trim())));
                        }
                    });
                    if let Some(e) = &self.error {
                        ui.add_space(8.0);
                        ui.label(RichText::new(e).color(BAD));
                    }
                    if let Err(e) = &self.audio {
                        ui.add_space(8.0);
                        ui.label(
                            RichText::new(format!(
                                "{}: {e}",
                                lang.t("Keine Wiedergabe", "No playback")
                            ))
                            .color(BAD),
                        );
                    }
                });
            });
        match open {
            Some(Ok(dir)) => self.open(ui.ctx(), &dir),
            Some(Err(e)) => self.error = Some(e),
            None => {}
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let lang = self.lang;
        let l = self.loaded.as_ref().expect("loaded");
        let p = &l.project;
        let sr = self.sample_rate();
        let tick = sample_tick(p, self.position(), sr);
        let bar = p.timing().pos(tick).bar;
        let playing = self.playing();
        let has_b = self.selected_prop().is_some_and(|p| p.preview.is_some());
        let mut toggle = false;
        egui::Panel::top("top")
            .frame(
                egui::Frame::NONE
                    .fill(PANEL)
                    .inner_margin(egui::Margin::symmetric(12, 8)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("PINNA").strong().color(ACCENT));
                    let icon = if playing { "■" } else { "▶" };
                    if ui
                        .add(egui::Button::new(icon).min_size(vec2(30.0, 26.0)))
                        .on_hover_text(lang.t("Abspielen/Stopp (Leertaste)", "Play/stop (space)"))
                        .clicked()
                    {
                        toggle = true;
                    }
                    ui.label(RichText::new(&p.name).strong());
                    ui.label(
                        RichText::new(format!(
                            "· {} BPM · {} {bar}",
                            trim_num(p.tempo),
                            lang.t("Takt", "Bar")
                        ))
                        .color(MUTED)
                        .strong(),
                    );
                    if let Some(name) = &self.loop_section
                        && ui
                            .add(
                                egui::Button::new(RichText::new(format!("⟲ {name}")).color(ACCENT))
                                    .frame(false),
                            )
                            .on_hover_text(lang.t("Schleife aus", "Loop off"))
                            .clicked()
                    {
                        actions.push(Action::Loop(name.clone()));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let next = if lang == Lang::De { Lang::En } else { Lang::De };
                        if ui
                            .button(if lang == Lang::De { "EN" } else { "DE" })
                            .clicked()
                        {
                            self.lang = next;
                        }
                        ui.add_enabled_ui(has_b, |ui| {
                            ui.spacing_mut().item_spacing.x = 0.0;
                            let b = self.hear_b && has_b;
                            if segment(ui, lang.t("B Vorschlag", "B proposal"), b).clicked() {
                                actions.push(Action::Hear(true));
                            }
                            if segment(ui, lang.t("A vorher", "A before"), !b).clicked() {
                                actions.push(Action::Hear(false));
                            }
                        });
                        ui.label(
                            RichText::new(lang.t("hören:", "hear:"))
                                .size(11.0)
                                .color(MUTED),
                        );
                        if !l.head.is_empty() {
                            pill(ui, &format!("git {} · {}", l.branch, l.head), MUTED);
                        }
                        match &self.audio {
                            Ok(a) => {
                                let n = a.player.underruns();
                                if n > 0 {
                                    pill(
                                        ui,
                                        &format!("{} {n}", lang.t("Aussetzer", "Dropouts")),
                                        BAD,
                                    );
                                }
                                // The left side needs its room in small windows.
                                if ui.available_width() > 420.0 {
                                    ui.label(RichText::new(&a.device).size(11.0).color(DIM));
                                }
                            }
                            Err(e) => {
                                ui.label(
                                    RichText::new(lang.t("keine Wiedergabe", "no playback"))
                                        .size(11.0)
                                        .color(BAD),
                                )
                                .on_hover_text(e);
                            }
                        }
                    });
                });
            });
        if toggle {
            self.toggle_play();
        }
    }

    fn side(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let lang = self.lang;
        let mut task = std::mem::take(&mut self.task);
        let mut revise = std::mem::take(&mut self.revise);
        let l = self.loaded.as_ref().expect("loaded");
        let busy = self.busy.is_some();
        egui::Panel::right("side")
            .exact_size(SIDE_W)
            .resizable(false)
            .frame(egui::Frame::NONE.fill(SIDE).stroke(Stroke::new(1.0, LINE)))
            .show(ui, |ui| {
                egui::Panel::top("side-head")
                    .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(12, 10)))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(lang.t("Vorschläge", "Proposals")).strong());
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                let n = l.open.len();
                                pill(ui, &format!("{n} {}", lang.t("zum Anhören", "to review")), if n > 0 { ACCENT } else { MUTED });
                            });
                        });
                    });
                egui::Panel::bottom("ask")
                    .frame(egui::Frame::NONE.inner_margin(egui::Margin::same(10)).stroke(Stroke::new(1.0, LINE)))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let w = ui.available_width() - 80.0;
                            let r = ui.add(egui::TextEdit::singleline(&mut task)
                                .hint_text(lang.t("Auftrag, z. B. „Pad im Intro breiter“", "Task, e.g. “wider pad in the intro”"))
                                .desired_width(w).margin(vec2(8.0, 6.0)));
                            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                            if (ui.add(primary(lang.t("Auftrag", "Send"))).clicked() || enter) && !task.trim().is_empty() {
                                actions.push(Action::Task(task.trim().to_string()));
                                task.clear();
                            }
                        });
                    });
                if let Some(p) = self.selected.as_deref().and_then(|n| l.open.iter().find(|p| p.info.name == n)) {
                    let gain = self.gains.get(&p.info.hash).copied();
                    egui::Panel::bottom("decide")
                        .frame(egui::Frame::NONE.fill(RAISED).inner_margin(egui::Margin::symmetric(12, 8)).stroke(Stroke::new(1.0, LINE)))
                        .show(ui, |ui| decide_bar(ui, lang, p, gain, busy, actions));
                }
                egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
                    egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        for (id, job) in &self.jobs {
                            job_item(ui, lang, *id, job, actions);
                        }
                        for p in &l.open {
                            let sel = self.selected.as_deref() == Some(p.info.name.as_str());
                            self.open_item(ui, p, sel, &mut revise, actions);
                        }
                        for p in &l.accepted {
                            accepted_item(ui, lang, p);
                        }
                        if self.jobs.is_empty() && l.open.is_empty() && l.accepted.is_empty() {
                            ui.add_space(16.0);
                            ui.vertical_centered(|ui| {
                                ui.label(RichText::new(lang.t(
                                    "Noch keine Vorschläge. Gib unten einen Auftrag, oder lass einen Agenten mit `pinna mcp` arbeiten.",
                                    "No proposals yet. Send a task below, or let an agent work through `pinna mcp`.",
                                )).color(MUTED));
                            });
                        }
                        for w in &l.warnings {
                            ui.label(RichText::new(w).size(11.0).color(BAD));
                        }
                    });
                });
            });
        self.task = task;
        self.revise = revise;
    }

    fn open_item(
        &self,
        ui: &mut egui::Ui,
        p: &Prop,
        sel: bool,
        revise: &mut String,
        actions: &mut Vec<Action>,
    ) {
        let lang = self.lang;
        let tracks = p.tracks().join(", ");
        let status = format!(
            "● {} · {tracks} · {} {}",
            lang.t("bereit", "ready"),
            p.changes.len(),
            if p.changes.len() == 1 {
                lang.t("Änderung", "change")
            } else {
                lang.t("Änderungen", "changes")
            }
        );
        let r = item_summary(ui, &p.info.title, &status, ACCENT, sel);
        if sel && self.reveal {
            r.scroll_to_me(Some(egui::Align::TOP));
        }
        if r.clicked() {
            actions.push(Action::Select(p.info.name.clone()));
        }
        if !sel {
            return;
        }
        egui::Frame::NONE
            .fill(RAISED)
            .inner_margin(egui::Margin {
                left: 12,
                right: 12,
                top: 0,
                bottom: 12,
            })
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 4.0;
                if !p.info.why.is_empty() {
                    ui.label(RichText::new(&p.info.why).color(SAY));
                }
                heading(ui, lang.t("ÄNDERUNGEN", "CHANGES"));
                for (track, what) in &p.changes {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new("•").color(MUTED));
                        ui.label(RichText::new(title_case(track)).color(MUTED));
                        ui.label(RichText::new(what).monospace().size(12.0));
                    });
                }
                heading(
                    ui,
                    lang.t(
                        "BELEG · VOM AGENTEN GEMESSEN",
                        "EVIDENCE · MEASURED BY THE AGENT",
                    ),
                );
                if !p.info.label.is_empty() {
                    ui.label(RichText::new(&p.info.label).size(11.0).color(MUTED));
                }
                evidence(ui, lang, &p.info.evidence);
                ui.horizontal(|ui| {
                    heading(ui, lang.t("LIVE-SPEKTRUM", "LIVE SPECTRUM"));
                    let b = self.hear_b;
                    pill(
                        ui,
                        if b {
                            lang.t("B · Vorschlag", "B · proposal")
                        } else {
                            lang.t("A · vorher", "A · before")
                        },
                        MUTED,
                    );
                });
                spectrum(ui, lang, &self.bands);
                ui.add_space(4.0);
                let r = ui.add(
                    egui::TextEdit::singleline(revise)
                        .hint_text(lang.t(
                            "Nachbessern, z. B. „weniger pumpen“",
                            "Revise, e.g. “less pumping”",
                        ))
                        .desired_width(f32::INFINITY)
                        .margin(vec2(8.0, 6.0)),
                );
                if r.lost_focus()
                    && ui.input(|i| i.key_pressed(Key::Enter))
                    && !revise.trim().is_empty()
                {
                    actions.push(Action::Revise(
                        p.info.name.clone(),
                        revise.trim().to_string(),
                    ));
                    revise.clear();
                }
            });
        separator(ui);
    }

    fn arrangement(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let lang = self.lang;
        let l = self.loaded.as_ref().expect("loaded");
        let p = &l.project;
        let sr = self.sample_rate();
        let t = p.timing();
        let end = p.end_tick().max(1);
        let pos = sample_tick(p, self.position(), sr);
        let jobs: Vec<agent::State> = self.jobs.iter().map(|(_, j)| j.snapshot()).collect();
        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(BG)
                    .inner_margin(egui::Margin::same(12)),
            )
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink(false)
                    .show(ui, |ui| {
                        let rows = p.tracks.len() as f32;
                        let height = RULER_H + rows * ROW_H + MASTER_H;
                        let width = ui.available_width();
                        let (frame_rect, _) =
                            ui.allocate_exact_size(vec2(width, height), Sense::hover());
                        let painter = ui.painter_at(frame_rect.expand(1.0));
                        painter.rect_filled(frame_rect, CornerRadius::same(8), PANEL);
                        let lane_x0 = frame_rect.left() + HEAD_W;
                        let lane_w = (frame_rect.width() - HEAD_W).max(1.0);
                        let x = |tick: u64| lane_x0 + tick as f32 / end as f32 * lane_w;
                        let bar_ticks = pinna_core::bar_ticks(&t);
                        let bars = end.div_ceil(bar_ticks);

                        // ruler
                        let ruler = Rect::from_min_size(
                            pos2(lane_x0, frame_rect.top()),
                            vec2(lane_w, RULER_H),
                        );
                        if let Some(ls) = self.loop_section.as_ref().and_then(|n| p.section(n)) {
                            let r = Rect::from_x_y_ranges(
                                x(t.tick(ls.from))..=x(t.tick(ls.to)),
                                ruler.y_range(),
                            );
                            painter.rect_filled(r, CornerRadius::ZERO, ACCENT.gamma_multiply(0.18));
                        }
                        let step = if lane_w / bars as f32 > 28.0 { 1 } else { 4 };
                        let starts: Vec<u64> = p.sections.iter().map(|s| t.tick(s.from)).collect();
                        for b in (0..bars).step_by(step) {
                            let tk = b * bar_ticks;
                            if starts.contains(&tk) {
                                continue;
                            }
                            painter.text(
                                pos2(x(tk) + 3.0, ruler.top() + 6.0),
                                Align2::LEFT_TOP,
                                format!("{}", b + 1),
                                FontId::proportional(10.0),
                                MUTED,
                            );
                        }
                        let ruler_resp =
                            ui.interact(ruler, ui.scope_id().with("ruler"), Sense::click());
                        let mut on_section = false;
                        for s in &p.sections {
                            let tk = t.tick(s.from);
                            let label = format!("{} {}", s.from.bar, s.name);
                            let galley = painter.layout_no_wrap(
                                label,
                                FontId::proportional(10.0),
                                Color32::PLACEHOLDER,
                            );
                            let r = Rect::from_min_size(
                                pos2(x(tk) + 1.0, ruler.top() + 3.0),
                                galley.size() + vec2(6.0, 4.0),
                            );
                            let resp = ui
                                .interact(r, ui.scope_id().with(("sec", &s.name)), Sense::click())
                                .on_hover_text(
                                    lang.t(
                                        "Klick: Abschnitt in Schleife",
                                        "Click: loop this section",
                                    ),
                                );
                            let looped = self.loop_section.as_deref() == Some(s.name.as_str());
                            let color = if looped {
                                ACCENT
                            } else if resp.hovered() {
                                TEXT
                            } else {
                                DIM.gamma_multiply(1.6)
                            };
                            if resp.hovered() {
                                on_section = true;
                            }
                            painter.galley(r.min + vec2(2.0, 1.0), galley, color);
                            if resp.clicked() {
                                actions.push(Action::Loop(s.name.clone()));
                            }
                        }
                        if !on_section
                            && ruler_resp.clicked()
                            && let Some(at) = ruler_resp.interact_pointer_pos()
                        {
                            let tick =
                                (((at.x - lane_x0) / lane_w).clamp(0.0, 1.0) * end as f32) as u64;
                            let snapped = tick / PPQ * PPQ;
                            actions.push(Action::Seek(t.sample(snapped, sr)));
                        }
                        painter.hline(frame_rect.x_range(), ruler.bottom(), Stroke::new(1.0, LINE));

                        // rows
                        let mut y = ruler.bottom();
                        let mut lanes: HashMap<&str, Rect> = HashMap::new();
                        for (i, track) in p.tracks.iter().enumerate() {
                            let row = Rect::from_min_size(
                                pos2(frame_rect.left(), y),
                                vec2(frame_rect.width(), ROW_H),
                            );
                            let lane = Rect::from_x_y_ranges(lane_x0..=row.right(), row.y_range());
                            grid(&painter, lane, bars, &x, bar_ticks);
                            let head = Rect::from_x_y_ranges(row.left()..=lane_x0, row.y_range());
                            painter.text(
                                head.left_top() + vec2(10.0, 9.0),
                                Align2::LEFT_TOP,
                                title_case(&track.id),
                                FontId::proportional(13.0),
                                TEXT,
                            );
                            let kind = track.instrument.as_ref().map_or_else(
                                || lang.t("Audio", "Audio").to_string(),
                                |d| format!("{:?}", d.kind),
                            );
                            let sub = if track.mute {
                                format!("{kind} · mute")
                            } else {
                                kind
                            };
                            painter.text(
                                head.left_top() + vec2(10.0, 28.0),
                                Align2::LEFT_TOP,
                                sub,
                                FontId::proportional(11.0),
                                MUTED,
                            );
                            let color = CLIPS[i % CLIPS.len()];
                            for c in &track.clips {
                                let (a, b) = c.span(&t);
                                let r = Rect::from_x_y_ranges(
                                    x(a)..=x(b),
                                    (row.top() + 8.0)..=(row.bottom() - 8.0),
                                )
                                .shrink2(vec2(0.5, 0.0));
                                let fill = if track.mute {
                                    color.gamma_multiply(0.35)
                                } else {
                                    color
                                };
                                painter.rect_filled(r, CornerRadius::same(5), fill);
                                clip_notes(&painter, r, c, &t);
                                let label_painter = painter.with_clip_rect(r);
                                label_painter.text(
                                    r.left_top() + vec2(6.0, 3.0),
                                    Align2::LEFT_TOP,
                                    c.id(),
                                    FontId::proportional(10.0),
                                    Color32::from_rgb(0x11, 0x11, 0x11),
                                );
                            }
                            painter.vline(lane_x0, row.y_range(), Stroke::new(1.0, LINE));
                            painter.hline(
                                frame_rect.x_range(),
                                row.bottom(),
                                Stroke::new(1.0, LINE),
                            );
                            lanes.insert(track.id.as_str(), lane);
                            y += ROW_H;
                        }
                        let row = Rect::from_min_size(
                            pos2(frame_rect.left(), y),
                            vec2(frame_rect.width(), MASTER_H),
                        );
                        painter.rect_filled(
                            row,
                            CornerRadius {
                                nw: 0,
                                ne: 0,
                                sw: 8,
                                se: 8,
                            },
                            MASTER_ROW,
                        );
                        painter.text(
                            row.left_top() + vec2(10.0, 12.0),
                            Align2::LEFT_TOP,
                            "Master",
                            FontId::proportional(13.0),
                            TEXT,
                        );
                        painter.vline(lane_x0, row.y_range(), Stroke::new(1.0, LINE));
                        lanes.insert(
                            MASTER,
                            Rect::from_x_y_ranges(lane_x0..=row.right(), row.y_range()),
                        );

                        // ghosts
                        // Newest first: one accepted ghost per track is enough.
                        let mut marked: Vec<&str> = Vec::new();
                        for prop in &l.accepted {
                            for id in prop.tracks() {
                                if marked.contains(&id) {
                                    continue;
                                }
                                marked.push(id);
                                if let Some(lane) = lanes.get(id) {
                                    let label =
                                        format!("✔ {} · {}", prop.info.title, prop.info.hash);
                                    ghost(
                                        ui,
                                        &painter,
                                        *lane,
                                        &x,
                                        prop.range,
                                        &label,
                                        Ghost::Done,
                                        None,
                                    );
                                }
                            }
                        }
                        for prop in &l.open {
                            let sel = self.selected.as_deref() == Some(prop.info.name.as_str());
                            for id in prop.tracks() {
                                let Some(lane) = lanes.get(id) else { continue };
                                let what: Vec<&str> = prop
                                    .changes
                                    .iter()
                                    .filter(|(t, _)| t == id)
                                    .map(|(_, w)| w.as_str())
                                    .take(2)
                                    .collect();
                                let label = format!("✏ {}", what.join(" · "));
                                let kind = if sel { Ghost::Selected } else { Ghost::Ready };
                                if ghost(
                                    ui,
                                    &painter,
                                    *lane,
                                    &x,
                                    prop.range,
                                    &label,
                                    kind,
                                    Some((&prop.info.name, id)),
                                ) {
                                    actions.push(Action::Reveal(prop.info.name.clone()));
                                }
                            }
                        }
                        for s in &jobs {
                            if s.done.is_some() {
                                continue;
                            }
                            let range = s
                                .range
                                .as_deref()
                                .and_then(|r| pinna_core::range::parse_range(p, r).ok())
                                .unwrap_or((0, end));
                            for id in &s.tracks {
                                if let Some(lane) = lanes.get(id.as_str()) {
                                    ghost(
                                        ui,
                                        &painter,
                                        *lane,
                                        &x,
                                        range,
                                        &format!("○ {}", s.activity),
                                        Ghost::Wait,
                                        None,
                                    );
                                }
                            }
                        }

                        // playhead
                        let px = x(pos.min(end));
                        painter.vline(
                            px,
                            frame_rect.top()..=frame_rect.bottom(),
                            Stroke::new(2.0, Color32::WHITE),
                        );
                        painter.rect_stroke(
                            frame_rect,
                            CornerRadius::same(8),
                            Stroke::new(1.0, LINE),
                            StrokeKind::Inside,
                        );
                    });
            });
    }

    fn toasts(&mut self, ctx: &egui::Context) {
        let Some((text, ok, at)) = &self.toast else {
            return;
        };
        if at.elapsed() > Duration::from_secs(if *ok { 4 } else { 8 }) {
            self.toast = None;
            return;
        }
        egui::Area::new(egui::Id::unique("toast"))
            .anchor(Align2::CENTER_TOP, vec2(0.0, 52.0))
            .show(ctx, |ui| {
                egui::Frame::NONE
                    .fill(if *ok { GOOD } else { BAD })
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(egui::Margin::symmetric(14, 8))
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(text.as_str())
                                .color(Color32::from_rgb(0x11, 0x11, 0x11))
                                .strong(),
                        );
                    });
            });
        ctx.request_repaint_after(Duration::from_millis(500));
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Ghost {
    Ready,
    Selected,
    Wait,
    Done,
}

/// A proposal's ghost on a lane. Returns true when clicked.
#[allow(clippy::too_many_arguments)]
fn ghost(
    ui: &egui::Ui,
    painter: &egui::Painter,
    lane: Rect,
    x: &impl Fn(u64) -> f32,
    range: (u64, u64),
    label: &str,
    kind: Ghost,
    id: Option<(&str, &str)>,
) -> bool {
    let r = Rect::from_x_y_ranges(
        x(range.0)..=x(range.1),
        (lane.top() + 3.0)..=(lane.bottom() - 3.0),
    );
    let (stroke, text, fill) = match kind {
        Ghost::Ready | Ghost::Selected => (ACCENT, ACCENT, GHOST_FILL),
        Ghost::Wait => (MUTED, SAY, GHOST_FILL),
        Ghost::Done => (GOOD, GOOD, GOOD.gamma_multiply(0.08)),
    };
    if kind == Ghost::Selected {
        painter.rect_stroke(
            r.expand(2.0),
            CornerRadius::same(8),
            Stroke::new(3.0, ACCENT.gamma_multiply(0.35)),
            StrokeKind::Outside,
        );
    }
    painter.rect_filled(r, CornerRadius::same(6), fill);
    if kind == Ghost::Done {
        painter.rect_stroke(
            r,
            CornerRadius::same(6),
            Stroke::new(1.5, stroke),
            StrokeKind::Inside,
        );
    } else {
        let s = r.shrink(1.0);
        let pts = [
            s.left_top(),
            s.right_top(),
            s.right_bottom(),
            s.left_bottom(),
            s.left_top(),
        ];
        painter.extend(Shape::dashed_line(&pts, Stroke::new(2.0, stroke), 6.0, 4.0));
    }
    painter.with_clip_rect(r.shrink(3.0)).text(
        r.left_top() + vec2(7.0, 5.0),
        Align2::LEFT_TOP,
        label,
        FontId::proportional(10.5),
        text,
    );
    match id {
        Some(id) => ui
            .interact(r, ui.scope_id().with(("ghost", id)), Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text(label)
            .clicked(),
        None => false,
    }
}

fn grid(painter: &egui::Painter, lane: Rect, bars: u64, x: &impl Fn(u64) -> f32, bar_ticks: u64) {
    for b in 1..bars {
        let c = if b % 4 == 0 {
            GRID.gamma_multiply(1.5)
        } else {
            GRID
        };
        if b % 4 == 0 || bars <= 64 {
            painter.vline(x(b * bar_ticks), lane.y_range(), Stroke::new(1.0, c));
        }
    }
}

/// Notes as small dashes inside a clip, pitch from bottom to top.
fn clip_notes(painter: &egui::Painter, r: Rect, clip: &Clip, t: &pinna_core::Timing) {
    let Clip::Notes {
        notes, loop_len, ..
    } = clip
    else {
        return;
    };
    if notes.is_empty() {
        return;
    }
    let (start, end) = clip.span(t);
    let span = loop_len.map_or(end - start, |l| l.ticks()).max(1);
    let (lo, hi) = notes
        .iter()
        .fold((u8::MAX, 0), |(a, b), n| (a.min(n.pitch), b.max(n.pitch)));
    let range = f32::from(hi - lo).max(1.0);
    let area = Rect::from_x_y_ranges(r.x_range(), (r.top() + 16.0)..=(r.bottom() - 4.0));
    if area.height() < 4.0 {
        return;
    }
    let w = r.width() / (end - start) as f32;
    let ink = Color32::from_rgba_premultiplied(17, 17, 17, 110);
    let mut loop_start = start;
    let mut drawn = 0;
    while loop_start < end && drawn < 4000 {
        for n in notes {
            let a = loop_start + n.at.ticks();
            if a >= end {
                continue;
            }
            let b = (a + n.len.ticks()).min(end);
            let yy = area.bottom() - f32::from(n.pitch - lo) / range * area.height();
            let x0 = r.left() + (a - start) as f32 * w;
            let x1 = (r.left() + (b - start) as f32 * w).max(x0 + 1.0);
            painter.hline(x0..=x1, yy, Stroke::new(2.0, ink));
            drawn += 1;
        }
        loop_start += span;
    }
}

fn item_summary(
    ui: &mut egui::Ui,
    title: &str,
    status: &str,
    color: Color32,
    sel: bool,
) -> egui::Response {
    let fill = if sel { RAISED } else { Color32::TRANSPARENT };
    let inner = egui::Frame::NONE
        .fill(fill)
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(RichText::new(title).color(TEXT));
            ui.label(RichText::new(status).size(11.0).color(color));
        });
    let r = inner
        .response
        .interact(Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    if sel {
        let rect = r.rect;
        ui.painter()
            .vline(rect.left() + 1.5, rect.y_range(), Stroke::new(3.0, ACCENT));
    }
    if !sel {
        separator(ui);
    }
    r
}

/// Fixed under the list, so deciding never scrolls out of view.
fn decide_bar(
    ui: &mut egui::Ui,
    lang: Lang,
    p: &Prop,
    gain: Option<f64>,
    busy: bool,
    actions: &mut Vec<Action>,
) {
    ui.horizontal(|ui| {
        let matched = match gain {
            Some(g) => format!(
                "{}\nB {} dB",
                lang.t("A/B pegelangeglichen", "A/B loudness-matched"),
                signed(g)
            ),
            None => lang
                .t("Pegel wird angeglichen …", "Matching loudness …")
                .to_string(),
        };
        ui.label(RichText::new(matched).size(11.0).color(MUTED));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_enabled_ui(!busy, |ui| {
                if ui.add(primary(lang.t("Annehmen", "Accept"))).clicked() {
                    actions.push(Action::Accept(p.info.name.clone()));
                }
                if ui.button(lang.t("Verwerfen", "Discard")).clicked() {
                    actions.push(Action::Discard(p.info.name.clone()));
                }
            });
        });
    });
}

fn job_item(ui: &mut egui::Ui, lang: Lang, id: u64, job: &Job, actions: &mut Vec<Action>) {
    let s = job.snapshot();
    let (status, color) = match &s.done {
        None => (
            format!("○ {} {}", lang.t("hört:", "listening:"), s.activity),
            MUTED,
        ),
        Some(Err(e)) => (format!("✕ {e}"), BAD),
        Some(Ok(a)) => (
            format!(
                "– {}{}",
                lang.t("kein Vorschlag", "no proposal"),
                if a.is_empty() {
                    String::new()
                } else {
                    format!(": {a}")
                }
            ),
            MUTED,
        ),
    };
    egui::Frame::NONE
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.horizontal(|ui| {
                ui.label(RichText::new(&job.task).color(TEXT));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                    if s.done.is_none() {
                        if ui.small_button(lang.t("Abbrechen", "Cancel")).clicked() {
                            actions.push(Action::Cancel(id));
                        }
                    } else if ui.small_button("×").clicked() {
                        actions.push(Action::Dismiss(id));
                    }
                });
            });
            ui.label(RichText::new(status).size(11.0).color(color));
        });
    separator(ui);
}

fn accepted_item(ui: &mut egui::Ui, lang: Lang, p: &Prop) {
    let status = format!(
        "✔ {} · Commit {}",
        lang.t("angenommen", "accepted"),
        p.info.hash
    );
    egui::Frame::NONE
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(RichText::new(&p.info.title).color(TEXT));
            ui.label(RichText::new(status).size(11.0).color(GOOD));
        });
    separator(ui);
}

fn heading(ui: &mut egui::Ui, text: &str) {
    ui.add_space(6.0);
    ui.label(RichText::new(text).size(10.0).strong().color(MUTED));
}

fn separator(ui: &mut egui::Ui) {
    let r = ui.available_rect_before_wrap();
    ui.painter()
        .hline(r.x_range(), r.top(), Stroke::new(1.0, LINE));
}

fn segment(ui: &mut egui::Ui, text: &str, on: bool) -> egui::Response {
    let rt = RichText::new(text);
    let b = if on {
        egui::Button::new(rt.color(Color32::from_rgb(0x11, 0x11, 0x11)).strong()).fill(ACCENT)
    } else {
        egui::Button::new(rt)
    };
    ui.add(b.corner_radius(CornerRadius::ZERO))
}

/// Evidence rows `name: a -> b (delta)` as label and value, notes muted, the band
/// differences as a small chart.
fn evidence(ui: &mut egui::Ui, lang: Lang, rows: &[String]) {
    for line in rows {
        let Some((name, value)) = line.split_once(": ") else {
            ui.label(RichText::new(line).size(11.0).color(MUTED));
            continue;
        };
        if name.starts_with("third-octave") {
            let values: Vec<f64> = value
                .split_whitespace()
                .filter_map(|v| v.rsplit(':').next()?.parse().ok())
                .collect();
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(lang.t("Terzbänder B − A, dB", "Third-octave B − A, dB"))
                        .size(12.0),
                );
            });
            band_diff(ui, &values);
            continue;
        }
        if !value.contains("->") && name.starts_with("masking") && !name.contains("worst") {
            ui.label(RichText::new(line).size(11.0).color(MUTED));
            continue;
        }
        let (label, better) = metric(lang, name);
        let (shown, delta) = match value.rsplit_once(" (") {
            Some((v, d)) => (v.to_string(), d.trim_end_matches(')').parse::<f64>().ok()),
            None => (value.to_string(), None),
        };
        // Unchanged measurements are noise here; loudness and peak always show.
        let unchanged = match delta {
            Some(d) => d.abs() < 0.005,
            None => value.split_once(" -> ").is_some_and(|(a, b)| a == b),
        };
        if unchanged && !matches!(name, "integrated LUFS" | "true peak dBTP") {
            continue;
        }
        let color = match (better, delta) {
            (Better::Lower, Some(d)) if d < 0.0 => GOOD,
            (Better::Lower, Some(d)) if d > 0.0 => BAD,
            (Better::Higher, Some(d)) if d > 0.0 => GOOD,
            (Better::Higher, Some(d)) if d < 0.0 => BAD,
            _ => TEXT,
        };
        let shown = local_numbers(lang, &shown.replace("->", "→"));
        let r = ui.horizontal(|ui| {
            ui.label(RichText::new(label).size(12.5));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let t = RichText::new(shown).monospace().color(color);
                ui.label(if color == TEXT { t } else { t.strong() });
            });
        });
        let y = r.response.rect.bottom() + 2.0;
        let pts = [
            pos2(r.response.rect.left(), y),
            pos2(r.response.rect.right(), y),
        ];
        ui.painter()
            .extend(Shape::dashed_line(&pts, Stroke::new(1.0, LINE), 3.0, 3.0));
    }
}

fn band_diff(ui: &mut egui::Ui, values: &[f64]) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, CornerRadius::same(6), INPUT);
    let mid = rect.center().y;
    p.hline(rect.x_range(), mid, Stroke::new(1.0, LINE));
    let n = values.len().max(1) as f32;
    let w = rect.width() / n;
    let max = values.iter().fold(3.0f64, |m, v| m.max(v.abs()));
    for (i, v) in values.iter().enumerate() {
        let h = (*v / max) as f32 * (rect.height() / 2.0 - 3.0);
        let x0 = rect.left() + i as f32 * w + 1.0;
        let r = Rect::from_x_y_ranges(
            x0..=(x0 + w - 2.0).max(x0 + 1.0),
            mid.min(mid - h)..=mid.max(mid - h),
        );
        p.rect_filled(
            r,
            CornerRadius::ZERO,
            if *v >= 0.0 {
                GOOD.gamma_multiply(0.8)
            } else {
                BAD.gamma_multiply(0.8)
            },
        );
    }
}

fn spectrum(ui: &mut egui::Ui, lang: Lang, bands: &[f64]) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 56.0), Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, CornerRadius::same(6), INPUT);
    if bands.is_empty() {
        let hint = lang.t(
            "▶ abspielen für das Live-Spektrum",
            "▶ play for the live spectrum",
        );
        p.text(
            rect.center(),
            Align2::CENTER_CENTER,
            hint,
            FontId::proportional(11.0),
            DIM,
        );
        return;
    }
    let w = rect.width() / bands.len() as f32;
    for (i, db) in bands.iter().enumerate() {
        let level = ((db + 80.0) / 80.0).clamp(0.0, 1.0) as f32;
        let h = level * (rect.height() - 6.0);
        let x0 = rect.left() + i as f32 * w + 1.0;
        let r = Rect::from_min_max(
            pos2(x0, rect.bottom() - 3.0 - h),
            pos2(x0 + w - 2.0, rect.bottom() - 3.0),
        );
        p.rect_filled(r, CornerRadius::same(1), ACCENT.gamma_multiply(0.85));
    }
}

/// Third-octave levels of the latest samples for the live display, dB. Cheaper and
/// coarser than `pinna_analysis::third_octave`: one Hann window, bands below the bin
/// spacing take their nearest bin.
fn live_bands(x: &[f32], sr: u32) -> Vec<f64> {
    let n = x.len().next_power_of_two() / 2;
    let x = &x[x.len() - n..];
    let fft = realfft::RealFftPlanner::<f64>::new().plan_fft_forward(n);
    let mut input: Vec<f64> = x
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let w = 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / n as f64).cos();
            f64::from(*v) * w
        })
        .collect();
    let mut spec = fft.make_output_vec();
    if fft.process(&mut input, &mut spec).is_err() {
        return Vec::new();
    }
    let bin = f64::from(sr) / n as f64;
    // Hann loses half the amplitude; a full-scale sine reads about 0 dB.
    let scale = 4.0 / (n as f64 * n as f64);
    pinna_analysis::THIRD_OCTAVE_HZ
        .iter()
        .map(|fc| {
            let (lo, hi) = (fc * 10f64.powf(-0.05), fc * 10f64.powf(0.05));
            let (a, b) = ((lo / bin).ceil() as usize, (hi / bin).floor() as usize);
            let (a, b) = if a > b {
                ((fc / bin).round() as usize, (fc / bin).round() as usize)
            } else {
                (a, b)
            };
            let power: f64 = spec
                .get(a..=b.min(spec.len() - 1))
                .unwrap_or(&[])
                .iter()
                .map(|c| c.norm_sqr() * scale)
                .sum();
            10.0 * power.max(1e-15).log10()
        })
        .collect()
}

/// Render A and B over a range and return the gain that matches B's loudness to A.
fn match_gain(a: &Project, b: &Project, root: &Path, range: (u64, u64)) -> f64 {
    let o = RenderOptions::default();
    let loud = |p: &Project| {
        pinna_engine::render(p, root, range.0, range.1, &o)
            .ok()
            .map(|r| pinna_analysis::loudness((&r.mix.l, &r.mix.r), r.sample_rate))
    };
    match (loud(a), loud(b)) {
        (Some(x), Some(y)) if x.integrated_lufs.is_finite() && y.integrated_lufs.is_finite() => {
            pinna_analysis::match_gain_db(&x, &y)
        }
        _ => 0.0,
    }
}

fn sample_tick(p: &Project, sample: u64, sr: u32) -> u64 {
    (sample as f64 / f64::from(sr) * p.tempo / 60.0 * PPQ as f64) as u64
}

fn example_dir() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .ok_or("no home folder")?;
    let dir = PathBuf::from(home).join("Pinna").join("nachtzug");
    if !dir.join("pinna.ron").exists() {
        pinna::project::init(&dir, Some("nachtzug"))?;
    }
    Ok(dir)
}

fn title_case(id: &str) -> String {
    let mut c = id.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

fn trim_num(x: f64) -> String {
    let s = format!("{x:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn signed(x: f64) -> String {
    let s = format!("{x:+.1}");
    s.replace('-', "−")
}

/// German decimal commas and real minus signs.
fn local_numbers(lang: Lang, s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        let digit = |j: usize| chars.get(j).is_some_and(|c| c.is_ascii_digit());
        match c {
            '.' if lang == Lang::De && i > 0 && digit(i - 1) && digit(i + 1) => out.push(','),
            '-' if digit(i + 1) || chars.get(i + 1) == Some(&'i') => out.push('−'),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_read_locally() {
        assert_eq!(local_numbers(Lang::De, "-10.4 → -10.3"), "−10,4 → −10,3");
        assert_eq!(local_numbers(Lang::En, "0.42 → 0.17"), "0.42 → 0.17");
        assert_eq!(local_numbers(Lang::De, "-inf → 1.5"), "−inf → 1,5");
    }

    #[test]
    fn live_bands_find_a_sine() {
        let sr = 48_000;
        let x: Vec<f32> = (0..8192)
            .map(|i| (std::f64::consts::TAU * 1000.0 * f64::from(i) / f64::from(sr)).sin() as f32)
            .collect();
        let b = live_bands(&x, sr);
        let peak = b.iter().enumerate().fold(
            (0, f64::MIN),
            |m, (i, v)| if *v > m.1 { (i, *v) } else { m },
        );
        assert_eq!(pinna_analysis::THIRD_OCTAVE_HZ[peak.0], 1000.0);
        assert!(
            (peak.1 + 3.0).abs() < 2.0,
            "1 kHz full scale reads {} dB",
            peak.1
        );
    }

    #[test]
    fn sample_to_tick() {
        let p =
            pinna_core::io::parse(include_str!("../../../examples/nachtzug/pinna.ron")).unwrap();
        let t = p.timing();
        let s = t.sample(t.tick("9.1".parse().unwrap()), 48_000);
        assert_eq!(t.pos(sample_tick(&p, s + 1, 48_000)).bar, 9);
    }
}
