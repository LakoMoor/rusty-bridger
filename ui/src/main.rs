#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use std::{
    fs,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
        Arc, Mutex,
    },
    thread,
};

use eframe::egui;
use rbridger_lib::{
    expr_app::ExprAppTracker,
    vtspc::{AfkConfig, CalcFn, VtsPc},
    vtsphone::{TrackingResponce, VtsPhone},
};

#[cfg(feature = "webcam")]
use rbridger_lib::webcam::{init_camera_permissions, PreviewFrame, WebcamTracker};

const APP_NAME:    &str = "RBridger";
const VERSION:     &str = env!("CARGO_PKG_VERSION");
const GITHUB_REPO: &str = "LakoMoor/RBridger";
const OVROG_TEMPLATE: &str = include_str!("../../configs/ovrog.json");

// ── Update checker ────────────────────────────────────────────────────────────

#[derive(Clone)]
struct UpdateInfo {
    version: String,
    url:     String,
}

fn parse_ver(v: &str) -> (u32, u32, u32) {
    let mut p = v.trim_start_matches('v').splitn(4, '.');
    let n = |s: Option<&str>| s.and_then(|x| x.parse().ok()).unwrap_or(0);
    (n(p.next()), n(p.next()), n(p.next()))
}

fn check_for_update() -> Option<UpdateInfo> {
    let url = format!("https://api.github.com/repos/{GITHUB_REPO}/releases/latest");
    let json: serde_json::Value = ureq::get(&url)
        .set("User-Agent", &format!("rbridger/{VERSION}"))
        .call().ok()?
        .into_json().ok()?;
    let tag  = json["tag_name"].as_str()?;
    let html = json["html_url"].as_str()?;
    let version = tag.trim_start_matches('v').to_string();
    if parse_ver(&version) > parse_ver(VERSION) {
        Some(UpdateInfo { version, url: html.to_string() })
    } else {
        None
    }
}

// ── Tracking source ───────────────────────────────────────────────────────────

#[derive(PartialEq, Clone, Copy, Debug, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "snake_case")]
enum TrackingSource { #[default] IPhone, ExprApp, Webcam }

// ── Persist config ────────────────────────────────────────────────────────────

fn app_dir() -> PathBuf {
    let base = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let dir  = base.join(".rusty-bridge");
    let _ = fs::create_dir_all(&dir);
    dir
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Default, Clone)]
#[serde(rename_all = "camelCase")]
struct Config {
    transform_path: Option<String>,
    ip:             Option<String>,
    source:         Option<TrackingSource>,
    expr_app_port:  Option<u16>,
    webcam_index:   Option<u32>,
    start_tab:      Option<String>,
    mirror:         Option<bool>,
    afk_mode:       Option<bool>,
    window_w:       Option<f32>,
    window_h:       Option<f32>,
}

impl Config {
    fn path() -> PathBuf { app_dir().join("ui-cfg.json") }
    fn load() -> Self {
        fs::read_to_string(Self::path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }
    fn save(&self) {
        if let Ok(s) = serde_json::to_string(self) {
            let _ = fs::write(Self::path(), s);
        }
    }
}

// ── App settings ──────────────────────────────────────────────────────────────

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
struct AppSettings {
    vts_port:              u16,
    auto_reconnect:        bool,
    reconnect_delay_secs:  u32,
    log_level:             String,
    theme:                 String,
    afk_timeout_secs:      u32,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            vts_port:             8001,
            auto_reconnect:       false,
            reconnect_delay_secs: 3,
            log_level:            "info".into(),
            theme:                "dark".into(),
            afk_timeout_secs:     3,
        }
    }
}

impl AppSettings {
    fn path() -> PathBuf { app_dir().join("settings.json") }
    fn load() -> Self {
        fs::read_to_string(Self::path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }
    fn save(&self) {
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let _ = fs::write(Self::path(), s);
        }
    }
}

// ── Tabs ──────────────────────────────────────────────────────────────────────

#[derive(PartialEq, Clone, Copy)]
enum Tab { Bridge, Config, Settings, About }

// ── Config editor ─────────────────────────────────────────────────────────────

#[derive(Default)]
struct Editor {
    params:      Vec<CalcFn>,
    selected:    Option<usize>,
    buf_name:    String,
    buf_func:    String,
    buf_min:     String,
    buf_max:     String,
    buf_default: String,
    formula_ok:  Option<bool>,
    name_dup:    bool,
    dirty:       bool,
    status:      String,
}

impl Editor {
    fn select(&mut self, idx: usize) {
        if idx >= self.params.len() { return; }
        let p = &self.params[idx];
        self.buf_name    = p.name.clone();
        self.buf_func    = p.func.clone();
        self.buf_min     = p.min.to_string();
        self.buf_max     = p.max.to_string();
        self.buf_default = p.default_value.to_string();
        self.selected    = Some(idx);
        self.validate_buffers();
    }

    fn validate_buffers(&mut self) {
        let f = self.buf_func.trim();
        self.formula_ok = if f.is_empty() { None } else {
            Some(evalexpr::build_operator_tree(f).is_ok())
        };
        let name = self.buf_name.trim();
        self.name_dup = self.params.iter().enumerate()
            .any(|(i, p)| Some(i) != self.selected && p.name == name);
    }

    fn apply_edit(&mut self) {
        let Some(idx) = self.selected else { return };
        let Some(p) = self.params.get_mut(idx) else { return };
        let name = self.buf_name.trim().to_string();
        if !name.is_empty() { p.name = name; }
        p.func          = self.buf_func.trim().to_string();
        p.min           = self.buf_min.parse().unwrap_or(p.min);
        p.max           = self.buf_max.parse().unwrap_or(p.max);
        p.default_value = self.buf_default.parse().unwrap_or(p.default_value);
        self.dirty = true;
    }

    fn add_param(&mut self) {
        self.apply_edit();
        let idx = self.params.len();
        self.params.push(CalcFn {
            name: format!("Param{}", idx + 1),
            func: "0".into(),
            min: -1.0, max: 1.0, default_value: 0.0,
        });
        self.dirty = true;
        self.select(idx);
    }

    fn delete_selected(&mut self) {
        let Some(idx) = self.selected else { return };
        self.params.remove(idx);
        self.dirty = true;
        self.selected = if self.params.is_empty() {
            None
        } else {
            let new = idx.min(self.params.len() - 1);
            self.select(new);
            Some(new)
        };
    }

    fn move_selected(&mut self, up: bool) {
        let Some(idx) = self.selected else { return };
        let new_idx = if up {
            if idx == 0 { return; } idx - 1
        } else {
            if idx + 1 >= self.params.len() { return; } idx + 1
        };
        self.apply_edit();
        self.params.swap(idx, new_idx);
        self.selected = Some(new_idx);
        self.dirty = true;
    }

    fn load_file(&mut self, path: &str) {
        match fs::read_to_string(path).and_then(|s| {
            serde_json::from_str::<Vec<CalcFn>>(&s)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
        }) {
            Ok(params) => {
                self.params   = params;
                self.selected = None;
                self.buf_name.clear(); self.buf_func.clear();
                self.dirty  = false;
                self.status = format!("Loaded {} params", self.params.len());
            }
            Err(e) => self.status = format!("Load error: {e}"),
        }
    }

    fn save_file(&mut self, path: &str) {
        self.apply_edit();
        match serde_json::to_string_pretty(&self.params) {
            Ok(s) => match fs::write(path, s) {
                Ok(_)  => { self.dirty = false; self.status = "Saved".into(); }
                Err(e) => self.status = format!("Save error: {e}"),
            },
            Err(e) => self.status = format!("Serialize error: {e}"),
        }
    }
}

// ── App ───────────────────────────────────────────────────────────────────────

struct App {
    cfg:             Config,
    settings:        AppSettings,
    settings_draft:  AppSettings,
    settings_status: String,
    tab:             Tab,
    transform_path:  String,
    // iPhone source
    phone_ip:        String,
    // ExprApp source
    source:          TrackingSource,
    expr_app_port:   String,
    // Webcam source
    webcam_index:    u32,
    webcam_cameras:  Vec<(u32, String)>,
    pending_cameras: Option<Receiver<Vec<(u32, String)>>>,
    // Webcam preview
    #[cfg(feature = "webcam")]
    webcam_preview:    Arc<Mutex<Option<PreviewFrame>>>,
    #[cfg(feature = "webcam")]
    webcam_frame_size: egui::Vec2,
    #[cfg(feature = "webcam")]
    webcam_lmks:       Vec<[f32; 2]>,
    show_preview:      bool,
    // Connection
    active:          Arc<AtomicBool>,
    vts_connected:   Arc<AtomicBool>,
    pending_path:    Option<Receiver<Option<String>>>,
    editor:          Editor,
    // Mirror & AFK
    mirror:          bool,
    afk_mode:        bool,
    // Window size persistence
    last_window_size: Option<egui::Vec2>,
    // Update checker
    update_rx:       Option<Receiver<Option<UpdateInfo>>>,
    update_info:     Option<UpdateInfo>,
    update_open:     bool,
}

impl App {
    fn new(cc: &eframe::CreationContext) -> Self {
        let cfg      = Config::load();
        let settings = AppSettings::load();
        apply_theme(&cc.egui_ctx, &settings.theme);
        setup_fonts(&cc.egui_ctx);
        let settings_draft = settings.clone();

        let (update_tx, update_rx) = mpsc::channel();
        thread::spawn(move || { let _ = update_tx.send(check_for_update()); });

        #[cfg(feature = "webcam")]
        init_camera_permissions();

        let (cam_tx, cam_rx) = mpsc::channel();
        thread::spawn(move || {
            #[cfg(feature = "webcam")]
            let cameras = WebcamTracker::list_cameras();
            #[cfg(not(feature = "webcam"))]
            let cameras: Vec<(u32, String)> = vec![];
            let _ = cam_tx.send(cameras);
        });

        let source = {
            let s = cfg.source.unwrap_or_default();
            #[cfg(not(target_os = "windows"))]
            let s = if s == TrackingSource::ExprApp { TrackingSource::IPhone } else { s };
            s
        };

        let mirror   = cfg.mirror.unwrap_or(false);
        let afk_mode = cfg.afk_mode.unwrap_or(false);

        Self {
            transform_path:  cfg.transform_path.clone().unwrap_or_default(),
            phone_ip:        cfg.ip.clone().unwrap_or_default(),
            source,
            expr_app_port:   cfg.expr_app_port.unwrap_or(9140).to_string(),
            webcam_index:    cfg.webcam_index.unwrap_or(0),
            webcam_cameras:  vec![],
            pending_cameras: Some(cam_rx),
            #[cfg(feature = "webcam")]
            webcam_preview:    Arc::new(Mutex::new(None)),
            #[cfg(feature = "webcam")]
            webcam_frame_size: egui::Vec2::new(320.0, 240.0),
            #[cfg(feature = "webcam")]
            webcam_lmks:       vec![],
            show_preview:      false,
            tab: match cfg.start_tab.as_deref() {
                Some("config")   => Tab::Config,
                Some("settings") => Tab::Settings,
                Some("about")    => Tab::About,
                _                => Tab::Bridge,
            },
            active:          Arc::new(AtomicBool::new(false)),
            vts_connected:   Arc::new(AtomicBool::new(false)),
            pending_path:    None,
            editor:          Editor::default(),
            settings_status: String::new(),
            settings_draft,
            settings,
            cfg,
            mirror,
            afk_mode,
            last_window_size: None,
            update_rx:   Some(update_rx),
            update_info: None,
            update_open: false,
        }
    }

    fn save_config(&mut self) {
        self.cfg.transform_path = Some(self.transform_path.clone());
        self.cfg.ip             = Some(self.phone_ip.clone());
        self.cfg.source         = Some(self.source);
        self.cfg.expr_app_port  = self.expr_app_port.parse().ok();
        self.cfg.webcam_index   = Some(self.webcam_index);
        self.cfg.mirror         = Some(self.mirror);
        self.cfg.afk_mode       = Some(self.afk_mode);
        self.cfg.save();
    }

    fn connect(&mut self) {
        self.active.store(true, Ordering::Relaxed);
        self.vts_connected.store(false, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel::<TrackingResponce>();
        let flag  = Arc::clone(&self.active);
        let flag2 = Arc::clone(&self.active);
        let path  = self.transform_path.clone();
        let mirror   = self.mirror;
        let afk = AfkConfig {
            enabled:      self.afk_mode,
            timeout_secs: self.settings.afk_timeout_secs,
        };

        match self.source {
            TrackingSource::IPhone => {
                let ip = self.phone_ip.clone();
                thread::spawn(move || VtsPhone::run(ip, tx, flag2));
            }
            TrackingSource::ExprApp => {
                let port = self.expr_app_port.parse::<u16>().unwrap_or(9140);
                thread::spawn(move || ExprAppTracker::run(port, tx, flag2));
            }
            TrackingSource::Webcam => {
                #[cfg(feature = "webcam")]
                {
                    let idx     = self.webcam_index;
                    let preview = Arc::clone(&self.webcam_preview);
                    self.show_preview = true;
                    thread::spawn(move || WebcamTracker::run(idx, tx, flag2, preview));
                }
                #[cfg(not(feature = "webcam"))]
                {
                    drop(tx); drop(flag2);
                }
            }
        }

        let vts_port = self.settings.vts_port;
        let vts_conn = Arc::clone(&self.vts_connected);
        thread::spawn(move || VtsPc::run(rx, path, flag, vts_port, vts_conn, mirror, afk));
    }

    fn disconnect(&mut self) {
        self.active.store(false, Ordering::Relaxed);
        self.vts_connected.store(false, Ordering::Relaxed);
    }

    fn can_connect(&self) -> bool {
        if self.transform_path.is_empty() { return false; }
        match self.source {
            TrackingSource::IPhone  => !self.phone_ip.is_empty(),
            TrackingSource::ExprApp => self.expr_app_port.parse::<u16>().is_ok(),
            TrackingSource::Webcam  => {
                #[cfg(feature = "webcam")] { true }
                #[cfg(not(feature = "webcam"))] { false }
            }
        }
    }

    fn open_file_dialog(&mut self) {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let r = rfd::FileDialog::new()
                .add_filter("JSON", &["json"])
                .pick_file()
                .map(|p| p.to_string_lossy().into_owned());
            let _ = tx.send(r);
        });
        self.pending_path = Some(rx);
    }
}

// ── eframe::App ───────────────────────────────────────────────────────────────

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        apply_theme(ctx, &self.settings_draft.theme);

        // Persist window size when it changes
        if let Some(rect) = ctx.input(|i| i.viewport().inner_rect) {
            let sz = rect.size();
            if self.last_window_size.map_or(true, |prev| (prev - sz).length() > 1.0) {
                self.last_window_size = Some(sz);
                self.cfg.window_w = Some(sz.x);
                self.cfg.window_h = Some(sz.y);
                self.cfg.save();
            }
        }

        // Sync vts_connected when phone/webcam thread stops the active flag
        if !self.active.load(Ordering::Relaxed) {
            self.vts_connected.store(false, Ordering::Relaxed);
        }

        // Live repaint while webcam preview is open
        #[cfg(feature = "webcam")]
        if self.source == TrackingSource::Webcam
            && self.active.load(Ordering::Relaxed)
            && self.show_preview
        {
            ctx.request_repaint_after(std::time::Duration::from_millis(33));
        }

        // Poll webcam preview frame
        #[cfg(feature = "webcam")]
        {
            let new_frame = self.webcam_preview
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take();
            if let Some(f) = new_frame {
                self.webcam_lmks       = f.landmarks.clone();
                self.webcam_frame_size = egui::Vec2::new(f.width as f32, f.height as f32);
            }
        }

        // Webcam preview floating window
        #[cfg(feature = "webcam")]
        if self.show_preview {
            let lmks       = self.webcam_lmks.clone();
            let connected  = self.active.load(Ordering::Relaxed);
            let frame_size = self.webcam_frame_size;
            egui::Window::new("📷 Face Preview")
                .open(&mut self.show_preview)
                .resizable(true)
                .default_size([320.0, 280.0])
                .min_size([180.0, 150.0])
                .show(ctx, |ui| {
                    if !connected {
                        ui.vertical_centered(|ui| {
                            ui.add_space(8.0);
                            ui.label(egui::RichText::new("Not connected")
                                .color(egui::Color32::from_gray(130)));
                        });
                        return;
                    }
                    if lmks.is_empty() {
                        ui.vertical_centered(|ui| {
                            ui.add_space(8.0);
                            ui.spinner();
                            ui.label(egui::RichText::new("Waiting for camera…")
                                .small().color(egui::Color32::from_gray(140)));
                        });
                        return;
                    }
                    let avail   = ui.available_size();
                    let scale   = (avail.x / frame_size.x).min(avail.y / frame_size.y);
                    let disp_sz = frame_size * scale;
                    let (rect, _) = ui.allocate_exact_size(disp_sz, egui::Sense::hover());
                    let painter   = ui.painter();
                    painter.rect_filled(rect, 4.0, egui::Color32::from_gray(18));
                    draw_face_mesh(painter, &lmks, rect, frame_size.x, frame_size.y);
                });
        }

        // Poll camera list
        if let Some(rx) = &self.pending_cameras {
            if let Ok(cams) = rx.try_recv() {
                self.webcam_cameras = cams;
                self.pending_cameras = None;
            }
        }

        // Poll update check
        if let Some(rx) = &self.update_rx {
            if let Ok(result) = rx.try_recv() {
                if let Some(info) = result {
                    self.update_info = Some(info);
                    self.update_open = true;
                }
                self.update_rx = None;
            }
        }

        // Update available popup
        if self.update_open {
            if let Some(info) = self.update_info.clone() {
                egui::Window::new("Update Available")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .open(&mut self.update_open)
                    .show(ctx, |ui| {
                        ui.add_space(4.0);
                        ui.vertical_centered(|ui| {
                            ui.label(egui::RichText::new(format!("Version {} is available!", info.version)).strong());
                            ui.add_space(2.0);
                            ui.label(egui::RichText::new(format!("You are running v{VERSION}"))
                                .small().color(egui::Color32::from_gray(150)));
                        });
                        ui.add_space(8.0);
                        ui.separator();
                        ui.add_space(6.0);
                        ui.vertical_centered(|ui| {
                            ui.hyperlink_to("Open release page", &info.url);
                        });
                        ui.add_space(4.0);
                    });
            }
        }

        // Poll async file dialog
        if let Some(rx) = self.pending_path.take() {
            match rx.try_recv() {
                Ok(Some(path)) => {
                    self.transform_path = path.clone();
                    self.save_config();
                    self.editor.load_file(&path);
                }
                Ok(None) => {}
                Err(_) => { self.pending_path = Some(rx); }
            }
        }

        let connected = self.active.load(Ordering::Relaxed);

        // ── Tab bar ────────────────────────────────────────────────────────
        egui::TopBottomPanel::top("tabs").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Bridge, "Bridge");
                let cfg_label = if self.editor.dirty { "Config ●" } else { "Config" };
                ui.selectable_value(&mut self.tab, Tab::Config, cfg_label);
                ui.selectable_value(&mut self.tab, Tab::Settings, "Settings");
                ui.selectable_value(&mut self.tab, Tab::About, "About");
            });
            ui.add_space(2.0);
        });

        // ── Status bar ─────────────────────────────────────────────────────
        egui::TopBottomPanel::bottom("statusbar").show(ctx, |ui| {
            ui.add_space(3.0);
            ui.horizontal(|ui| {
                let (col, txt) = if !connected {
                    (egui::Color32::from_rgb(200, 60, 60), "Disconnected")
                } else if self.vts_connected.load(Ordering::Relaxed) {
                    (egui::Color32::from_rgb(60, 200, 90), "Connected")
                } else {
                    (egui::Color32::from_rgb(220, 185, 0), "Connecting\u{2026}")
                };
                ui.label(egui::RichText::new("●").color(col).small());
                ui.label(egui::RichText::new(txt).small());

                if !self.transform_path.is_empty() {
                    ui.separator();
                    let fname = PathBuf::from(&self.transform_path)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    ui.label(egui::RichText::new(fname).small().color(egui::Color32::from_gray(150)));
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(egui::RichText::new(format!("v{VERSION}"))
                        .small().color(egui::Color32::from_gray(80)));
                });
            });
            ui.add_space(3.0);
        });

        // ── Central panel ──────────────────────────────────────────────────
        egui::CentralPanel::default().show(ctx, |ui| {
            match self.tab {
                Tab::Bridge   => bridge_ui(ui, self, connected),
                Tab::Config   => config_editor_ui(ui, &mut self.editor, &mut self.transform_path, &mut self.cfg),
                Tab::Settings => settings_ui(ui, &mut self.settings, &mut self.settings_draft, &mut self.settings_status),
                Tab::About    => about_ui(ui, &mut self.update_rx, &self.update_info, &mut self.update_open),
            }
        });
    }
}

// ── Theme & fonts ─────────────────────────────────────────────────────────────

fn apply_theme(ctx: &egui::Context, theme: &str) {
    match theme {
        "light" => ctx.set_visuals(egui::Visuals::light()),
        _       => ctx.set_visuals(egui::Visuals::dark()),
    }
}

fn setup_fonts(ctx: &egui::Context) {
    ctx.style_mut(|style| {
        // Slightly larger body text for readability
        for (text_style, font_id) in style.text_styles.iter_mut() {
            match text_style {
                egui::TextStyle::Body | egui::TextStyle::Button => {
                    font_id.size = 14.0;
                }
                egui::TextStyle::Heading => {
                    font_id.size = 16.0;
                }
                egui::TextStyle::Small => {
                    font_id.size = 12.0;
                }
                _ => {}
            }
        }
        style.spacing.item_spacing   = egui::vec2(8.0, 4.0);
        style.spacing.button_padding = egui::vec2(8.0, 4.0);
    });
}

// ── Face landmark mesh ────────────────────────────────────────────────────────
#[cfg(feature = "webcam")]
fn draw_face_mesh(painter: &egui::Painter, pts: &[[f32; 2]], rect: egui::Rect, fw: f32, fh: f32) {
    let n = pts.len();
    if n < 106 { return; }

    let p = |i: usize| egui::pos2(
        rect.min.x + (pts[i][0] / fw) * rect.width(),
        rect.min.y + (pts[i][1] / fh) * rect.height(),
    );

    let s0  = egui::Stroke::new(0.6, egui::Color32::from_rgba_unmultiplied(150, 130, 110, 110));
    let md2 = (fw * 0.18) * (fw * 0.18);
    for i in 0..n {
        for j in (i + 1)..n {
            let dx = pts[i][0] - pts[j][0];
            let dy = pts[i][1] - pts[j][1];
            if dx * dx + dy * dy < md2 {
                painter.line_segment([p(i), p(j)], s0);
            }
        }
    }

    let edge = |a: usize, b: usize, s: egui::Stroke| {
        if a < n && b < n { painter.line_segment([p(a), p(b)], s); }
    };

    let sr = egui::Stroke::new(1.5, egui::Color32::from_rgba_unmultiplied(220,  55,  55, 235));
    let sl = egui::Stroke::new(1.5, egui::Color32::from_rgba_unmultiplied( 55, 210,  55, 235));
    let sm = egui::Stroke::new(1.5, egui::Color32::from_rgba_unmultiplied(245, 245, 245, 215));
    let sc = egui::Stroke::new(0.9, egui::Color32::from_rgba_unmultiplied(180, 165, 145, 160));

    const OVAL: &[usize] = &[
        17, 25, 26, 27, 28, 29, 30, 31, 32,
        18, 19, 20, 21, 22, 23, 24,
        0,
        8,  7,  6,  5,  4,  3,  2,
        16, 15, 14, 13, 12, 11, 10, 9, 1,
    ];
    for w in OVAL.windows(2) { edge(w[0], w[1], sc); }

    for k in 33..42 { edge(k, k + 1, sr); }
    for k in 43..51 { edge(k, k + 1, sr); }
    edge(51, 43, sr);

    for k in 87..96 { edge(k, k + 1, sl); }
    for k in 97..105 { edge(k, k + 1, sl); }
    edge(105, 97, sl);

    for k in 52..63 { edge(k, k + 1, sm); }
    edge(63, 52, sm);
    for k in 64..71 { edge(k, k + 1, sm); }
    edge(71, 64, sm);

    for k in 72..86 { edge(k, k + 1, sc); }

    let cd = egui::Color32::from_rgba_unmultiplied(200, 100, 55, 195);
    for i in 0..n { painter.circle_filled(p(i), 1.5, cd); }
}

// ── Bridge tab ────────────────────────────────────────────────────────────────

fn bridge_ui(ui: &mut egui::Ui, app: &mut App, connected: bool) {
    ui.add_space(8.0);

    // ── Source selector ────────────────────────────────────────────────────
    ui.label(egui::RichText::new("Tracking source").small().color(egui::Color32::from_gray(140)));
    ui.add_space(3.0);
    ui.horizontal(|ui| {
        let changed_to = {
            let mut new_src = None;
            if ui.add(egui::SelectableLabel::new(
                app.source == TrackingSource::IPhone,
                egui::RichText::new("📱 iPhone"),
            )).clicked() { new_src = Some(TrackingSource::IPhone); }

            #[cfg(target_os = "windows")]
            if ui.add(egui::SelectableLabel::new(
                app.source == TrackingSource::ExprApp,
                egui::RichText::new("⚡ ExpressionApp"),
            )).clicked() { new_src = Some(TrackingSource::ExprApp); }

            let webcam_label = egui::RichText::new("🎥 Webcam ⚠");
            #[cfg(not(feature = "webcam"))]
            let webcam_label = webcam_label.color(egui::Color32::from_gray(90));
            if ui.add_enabled(
                cfg!(feature = "webcam"),
                egui::SelectableLabel::new(app.source == TrackingSource::Webcam, webcam_label),
            ).clicked() { new_src = Some(TrackingSource::Webcam); }

            new_src
        };
        if let Some(src) = changed_to {
            if !connected { app.source = src; app.save_config(); }
        }
    });

    ui.add_space(8.0);
    ui.separator();
    ui.add_space(8.0);

    // ── Transform config ───────────────────────────────────────────────────
    ui.horizontal(|ui| {
        let btn_w = 36.0;
        let field_w = (ui.available_width() - btn_w - 6.0).max(80.0);
        let r = ui.add_sized(
            [field_w, 22.0],
            egui::TextEdit::singleline(&mut app.transform_path)
                .hint_text("Transform config (.json)")
                .interactive(!connected),
        );
        if r.changed() { app.save_config(); }
        if ui.add_enabled(!connected, egui::Button::new("📂").min_size([btn_w, 22.0].into()))
            .on_hover_text("Browse…").clicked() {
            app.open_file_dialog();
        }
    });

    ui.add_space(6.0);

    // ── Source-specific inputs ─────────────────────────────────────────────
    match app.source {
        TrackingSource::IPhone => {
            let r = ui.add_sized(
                [ui.available_width(), 22.0],
                egui::TextEdit::singleline(&mut app.phone_ip)
                    .hint_text("iPhone IP  (e.g. 192.168.1.10)")
                    .interactive(!connected),
            );
            if r.changed() { app.save_config(); }
        }

        TrackingSource::ExprApp => {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("UDP Port").small().color(egui::Color32::from_gray(160)));
                let r = ui.add_sized(
                    [70.0, 22.0],
                    egui::TextEdit::singleline(&mut app.expr_app_port)
                        .hint_text("9140")
                        .interactive(!connected),
                );
                if r.changed() { app.save_config(); }
            });
            ui.add_space(4.0);
            egui::Frame::none()
                .fill(egui::Color32::from_rgb(30, 45, 65))
                .inner_margin(egui::Margin::symmetric(8.0, 5.0))
                .rounding(4.0)
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(egui::RichText::new("ℹ").color(egui::Color32::from_rgb(100, 160, 255)));
                        ui.label(egui::RichText::new(
                            "Requires VTube Studio NVIDIA or MediaPipe Webcam Tracker DLC. \
                             Launch ExpressionApp.exe from the MXTracker folder first."
                        ).small().color(egui::Color32::from_gray(180)));
                    });
                    ui.add_space(2.0);
                    ui.label(egui::RichText::new(
                        "Use ARKit names in your transform config formulas \
                         (e.g. eyeBlink_L, jawOpen, mouthSmile_L)."
                    ).small().color(egui::Color32::from_gray(140)));
                });
        }

        TrackingSource::Webcam => {
            egui::Frame::none()
                .fill(egui::Color32::from_rgb(60, 40, 10))
                .inner_margin(egui::Margin::symmetric(8.0, 5.0))
                .rounding(4.0)
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(egui::RichText::new("⚠").color(egui::Color32::from_rgb(255, 180, 30)));
                        ui.label(egui::RichText::new(
                            "EXPERIMENTAL — built-in webcam tracker is in development. \
                             Accuracy is lower than native VTube Studio trackers."
                        ).small().color(egui::Color32::from_rgb(230, 200, 140)));
                    });
                });

            ui.add_space(6.0);

            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Camera").small().color(egui::Color32::from_gray(160)));

                if app.pending_cameras.is_some() {
                    ui.spinner();
                    ui.label(egui::RichText::new("detecting…").small().color(egui::Color32::from_gray(130)));
                } else if app.webcam_cameras.is_empty() {
                    ui.label(egui::RichText::new("Index:").small().color(egui::Color32::from_gray(160)));
                    let mut idx_str = app.webcam_index.to_string();
                    let r = ui.add_sized([50.0, 22.0],
                        egui::TextEdit::singleline(&mut idx_str)
                            .interactive(!connected));
                    if r.changed() {
                        if let Ok(v) = idx_str.parse() {
                            app.webcam_index = v;
                            app.save_config();
                        }
                    }
                    if ui.small_button("↺").on_hover_text("Re-detect cameras").clicked() {
                        let (tx, rx) = mpsc::channel();
                        thread::spawn(move || {
                            #[cfg(feature = "webcam")]
                            let cameras = WebcamTracker::list_cameras();
                            #[cfg(not(feature = "webcam"))]
                            let cameras: Vec<(u32, String)> = vec![];
                            let _ = tx.send(cameras);
                        });
                        app.pending_cameras = Some(rx);
                    }
                } else {
                    let selected_name = app.webcam_cameras.iter()
                        .find(|(i, _)| *i == app.webcam_index)
                        .map(|(_, n)| n.clone())
                        .unwrap_or_else(|| format!("Camera {}", app.webcam_index));
                    let cameras = app.webcam_cameras.clone();
                    egui::ComboBox::from_id_salt("cam_select")
                        .selected_text(&selected_name)
                        .width(ui.available_width() - 30.0)
                        .show_ui(ui, |ui| {
                            for (idx, name) in &cameras {
                                let label = format!("[{idx}] {name}");
                                if ui.selectable_value(&mut app.webcam_index, *idx, &label).changed() {
                                    app.save_config();
                                }
                            }
                        });
                    if ui.small_button("↺").on_hover_text("Re-detect cameras").clicked() {
                        let (tx, rx) = mpsc::channel();
                        thread::spawn(move || {
                            #[cfg(feature = "webcam")]
                            let cameras = WebcamTracker::list_cameras();
                            #[cfg(not(feature = "webcam"))]
                            let cameras: Vec<(u32, String)> = vec![];
                            let _ = tx.send(cameras);
                        });
                        app.pending_cameras = Some(rx);
                    }
                }
            });
        }
    }

    ui.add_space(12.0);

    // ── Connect / Disconnect ───────────────────────────────────────────────
    let label = if connected { "Disconnect" } else { "Connect" };
    if ui.add_enabled(
        connected || app.can_connect(),
        egui::Button::new(label).min_size([ui.available_width(), 32.0].into()),
    ).clicked() {
        if connected { app.disconnect(); } else { app.connect(); }
    }

    ui.add_space(6.0);

    // Connection hints
    if !connected {
        let hint = if app.transform_path.is_empty() {
            Some("① Browse or paste a transform config path")
        } else {
            match app.source {
                TrackingSource::IPhone if app.phone_ip.is_empty() =>
                    Some("② Enter your iPhone's IP address"),
                TrackingSource::IPhone =>
                    Some("③ Press Connect — make sure VTube Studio is open"),
                TrackingSource::ExprApp =>
                    Some("② Launch ExpressionApp.exe, then press Connect"),
                TrackingSource::Webcam =>
                    Some("② Select a camera, then press Connect"),
            }
        };
        if let Some(h) = hint {
            ui.label(egui::RichText::new(h).small().color(egui::Color32::from_gray(130)));
        }
    }

    // Preview toggle — shown only for webcam source
    #[cfg(feature = "webcam")]
    if app.source == TrackingSource::Webcam {
        ui.add_space(4.0);
        let btn_label = if app.show_preview { "📷 Hide Preview" } else { "📷 Show Preview" };
        if ui.add_enabled(
            connected,
            egui::Button::new(btn_label).min_size([ui.available_width(), 24.0].into()),
        ).on_disabled_hover_text("Connect first to see the preview")
         .clicked()
        {
            app.show_preview = !app.show_preview;
        }
    }

    ui.add_space(10.0);
    ui.separator();
    ui.add_space(6.0);

    // ── Mirror & AFK options ───────────────────────────────────────────────
    ui.horizontal(|ui| {
        if ui.checkbox(&mut app.mirror, "Mirror L/R").changed() {
            app.save_config();
        }
        ui.label(egui::RichText::new("Flip head yaw, eyes, and body horizontal")
            .small().color(egui::Color32::from_gray(120)));
    });

    ui.add_space(3.0);

    ui.horizontal(|ui| {
        if ui.checkbox(&mut app.afk_mode, "AFK detection").changed() {
            app.save_config();
        }
        let timeout = app.settings.afk_timeout_secs;
        ui.label(egui::RichText::new(
            format!("Tell VTS face lost after {timeout} s away (Settings → AFK timeout)")
        ).small().color(egui::Color32::from_gray(120)));
    });

    ui.add_space(6.0);
    ui.separator();
    ui.add_space(6.0);
    ui.label(egui::RichText::new("github.com/LakoMoor/RBridger")
        .small().color(egui::Color32::from_gray(85)));
}

// ── Config editor tab ─────────────────────────────────────────────────────────

fn config_editor_ui(
    ui: &mut egui::Ui,
    ed: &mut Editor,
    path: &mut String,
    cfg: &mut Config,
) {
    use egui::{Color32, RichText};

    // ── Toolbar ────────────────────────────────────────────────────────────────
    ui.horizontal(|ui| {
        if ui.button("📂 Load").clicked() {
            if let Some(p) = rfd::FileDialog::new().add_filter("JSON", &["json"]).pick_file() {
                let s = p.to_string_lossy().into_owned();
                *path = s.clone();
                cfg.transform_path = Some(s.clone());
                cfg.save();
                ed.load_file(&s);
            }
        }
        if ui.button("💾 Save").clicked() {
            if path.is_empty() {
                if let Some(p) = rfd::FileDialog::new().add_filter("JSON", &["json"]).save_file() {
                    *path = p.to_string_lossy().into_owned();
                    cfg.transform_path = Some(path.clone());
                    cfg.save();
                }
            }
            if !path.is_empty() { ed.save_file(path); }
        }
        if ui.button("📋 Template").on_hover_text("Load ovrog default config").clicked() {
            match serde_json::from_str::<Vec<CalcFn>>(OVROG_TEMPLATE) {
                Ok(params) => {
                    ed.params   = params;
                    ed.selected = None;
                    ed.dirty    = true;
                    ed.status   = format!("Template loaded ({} params) — save when ready", ed.params.len());
                }
                Err(e) => { ed.status = format!("Template error: {e}"); }
            }
        }
        ui.separator();
        let has       = ed.selected.is_some();
        let not_first = ed.selected.map_or(false, |i| i > 0);
        let not_last  = ed.selected.map_or(false, |i| i + 1 < ed.params.len());
        if ui.button("＋ Add").clicked() { ed.add_param(); }
        if ui.add_enabled(has && not_first, egui::Button::new("↑")).clicked() { ed.move_selected(true); }
        if ui.add_enabled(has && not_last,  egui::Button::new("↓")).clicked() { ed.move_selected(false); }
        if ui.add_enabled(has, egui::Button::new("🗑 Delete")).clicked() { ed.delete_selected(); }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let col = if ed.status.contains("error") || ed.status.contains("Error") {
                Color32::from_rgb(220, 80, 80)
            } else {
                Color32::from_gray(150)
            };
            ui.label(RichText::new(&ed.status).small().color(col));
        });
    });

    ui.separator();

    // ── Main area: param list + editor ─────────────────────────────────────────
    let avail_h = ui.available_height();
    let avail_w = ui.available_width();

    // Left panel width: 38% of available, clamped to [130, 220]
    let left_w = (avail_w * 0.38).clamp(130.0, 220.0);
    let right_w = (avail_w - left_w - 12.0).max(100.0);

    ui.horizontal(|ui| {
        // Left: scrollable param list
        ui.vertical(|ui| {
            ui.set_width(left_w);
            egui::ScrollArea::vertical()
                .id_salt("param_list")
                .max_height(avail_h)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.set_min_width(left_w - 8.0);
                    for i in 0..ed.params.len() {
                        let sel = ed.selected == Some(i);
                        let col = if sel { Color32::WHITE } else { Color32::from_gray(210) };
                        if ui.selectable_label(
                            sel,
                            RichText::new(&ed.params[i].name).monospace().small().color(col),
                        ).clicked() && !sel {
                            ed.apply_edit();
                            ed.select(i);
                        }
                    }
                    if ed.params.is_empty() {
                        ui.add_space(12.0);
                        ui.label(RichText::new("No params yet.\n\nPress 📋 Template to\nload a default config,\nor ＋ Add to create one.")
                            .small().color(Color32::from_gray(120)));
                    }
                });
        });

        ui.separator();

        // Right: edit form + variables reference
        egui::ScrollArea::vertical()
            .id_salt("editor_right")
            .max_height(avail_h)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if ed.selected.is_none() {
                    ui.add_space(24.0);
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new("← Select a param to edit").color(Color32::from_gray(140)));
                    });
                } else {
                    let w = right_w;

                    ui.label(RichText::new("Name").small().color(Color32::from_gray(150)));
                    ui.horizontal(|ui| {
                        let r = ui.add_sized([(w - 60.0).max(60.0), 22.0], egui::TextEdit::singleline(&mut ed.buf_name));
                        if r.changed() { ed.apply_edit(); ed.validate_buffers(); }
                        if ed.name_dup {
                            ui.label(RichText::new("⚠ dup").small().color(Color32::from_rgb(240, 160, 30)));
                        }
                    });

                    ui.add_space(5.0);

                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Formula").small().color(Color32::from_gray(150)));
                        match ed.formula_ok {
                            Some(true)  => { ui.label(RichText::new("✓").color(Color32::from_rgb(80, 200, 100))); }
                            Some(false) => { ui.label(RichText::new("✗").color(Color32::from_rgb(220, 80, 80))); }
                            None => {}
                        }
                    });
                    let r = ui.add_sized(
                        [w, 60.0],
                        egui::TextEdit::multiline(&mut ed.buf_func)
                            .font(egui::TextStyle::Monospace)
                            .hint_text("e.g.  JawOpen\n      HeadRotY * -1"),
                    );
                    if r.changed() { ed.apply_edit(); ed.validate_buffers(); }

                    ui.add_space(5.0);

                    ui.label(RichText::new("Range").small().color(Color32::from_gray(150)));
                    let fw = ((w - 86.0) / 3.0).max(30.0);
                    ui.horizontal(|ui| {
                        let mut changed = false;
                        ui.label(RichText::new("min").small().color(Color32::from_gray(160)));
                        changed |= ui.add_sized([fw, 20.0], egui::TextEdit::singleline(&mut ed.buf_min)).changed();
                        ui.label(RichText::new("max").small().color(Color32::from_gray(160)));
                        changed |= ui.add_sized([fw, 20.0], egui::TextEdit::singleline(&mut ed.buf_max)).changed();
                        ui.label(RichText::new("def").small().color(Color32::from_gray(160)));
                        changed |= ui.add_sized([fw, 20.0], egui::TextEdit::singleline(&mut ed.buf_default)).changed();
                        if changed { ed.apply_edit(); ed.validate_buffers(); }
                    });

                    let min_v: f64 = ed.buf_min.parse().unwrap_or(-1.0);
                    let max_v: f64 = ed.buf_max.parse().unwrap_or(1.0);
                    let def_v: f64 = ed.buf_default.parse().unwrap_or(0.0);
                    let t = if (max_v - min_v).abs() > 1e-9 {
                        ((def_v - min_v) / (max_v - min_v)).clamp(0.0, 1.0) as f32
                    } else { 0.5 };
                    ui.add_space(3.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("{:.1}", min_v)).small().monospace().color(Color32::from_gray(150)));
                        let bw = (ui.available_width() - 30.0).max(10.0);
                        let (rect, _) = ui.allocate_exact_size(egui::vec2(bw, 7.0), egui::Sense::hover());
                        ui.painter().rect_filled(rect, 3.0, Color32::from_gray(45));
                        let fill = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width() * t, rect.height()));
                        ui.painter().rect_filled(fill, 3.0, Color32::from_rgb(70, 145, 230));
                        let tx = rect.min.x + rect.width() * t;
                        ui.painter().line_segment(
                            [egui::pos2(tx, rect.min.y - 2.0), egui::pos2(tx, rect.max.y + 2.0)],
                            egui::Stroke::new(1.5, Color32::WHITE),
                        );
                        ui.label(RichText::new(format!("{:.1}", max_v)).small().monospace().color(Color32::from_gray(150)));
                    });
                }

                // ── Variables reference ──────────────────────────────────────
                ui.add_space(10.0);
                ui.separator();
                ui.add_space(4.0);
                ui.label(RichText::new("Variables reference").small().strong().color(Color32::from_gray(180)));
                ui.add_space(3.0);
                let vars: &[(&str, &str)] = &[
                    ("Head",   "HeadRotX  HeadRotY  HeadRotZ\nHeadPosX  HeadPosY  HeadPosZ"),
                    ("Eyes",   "EyeBlinkLeft/Right   EyeWideLeft/Right\nEyeSquintLeft/Right"),
                    ("Gaze",   "EyeLookUpLeft/Right  EyeLookDownLeft/Right\nEyeLookInLeft/Right  EyeLookOutLeft/Right"),
                    ("Brows",  "BrowOuterUpLeft/Right  BrowDownLeft/Right\nBrowInnerUp"),
                    ("Mouth",  "JawOpen  MouthSmileLeft/Right\nMouthFrownLeft/Right  MouthLeft  MouthRight\nMouthFunnel  MouthPucker  MouthRollLower/Upper\nMouthShrugUpper/Lower  MouthDimpleLeft/Right\nMouthUpperUpLeft/Right  MouthLowerDownLeft/Right\nMouthClose  MouthPressLeft/Right"),
                    ("Other",  "CheekPuff  TongueOut\nNoseSneerLeft/Right\nmath::abs  math::sqrt  math::sin  …"),
                ];
                egui::Grid::new("vars_ref").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
                    for (cat, names) in vars {
                        ui.label(RichText::new(*cat).small().strong().color(Color32::from_gray(160)));
                        ui.label(RichText::new(*names).small().monospace().color(Color32::from_gray(195)));
                        ui.end_row();
                    }
                });
            });
    });
}

// ── Settings tab ──────────────────────────────────────────────────────────────

fn settings_ui(
    ui: &mut egui::Ui,
    settings: &mut AppSettings,
    draft: &mut AppSettings,
    status: &mut String,
) {
    ui.add_space(4.0);
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.label(egui::RichText::new("Connection").strong());
        ui.separator();
        egui::Grid::new("conn_grid").num_columns(2).spacing([12.0, 6.0]).min_col_width(120.0).show(ui, |ui| {
            ui.label("VTube Studio port");
            let mut port_str = draft.vts_port.to_string();
            if ui.add_sized([80.0, 20.0], egui::TextEdit::singleline(&mut port_str).hint_text("8001")).changed() {
                if let Ok(p) = port_str.parse::<u16>() { draft.vts_port = p; }
            }
            ui.end_row();
            ui.label("Auto-reconnect");
            ui.checkbox(&mut draft.auto_reconnect, "");
            ui.end_row();
            ui.add_enabled(draft.auto_reconnect, egui::Label::new("Reconnect delay (s)"));
            ui.add_enabled(draft.auto_reconnect,
                egui::Slider::new(&mut draft.reconnect_delay_secs, 1..=30).suffix("s"));
            ui.end_row();
        });

        ui.add_space(10.0);
        ui.label(egui::RichText::new("AFK detection").strong());
        ui.separator();
        egui::Grid::new("afk_grid").num_columns(2).spacing([12.0, 6.0]).min_col_width(120.0).show(ui, |ui| {
            ui.label("AFK timeout");
            ui.add(egui::Slider::new(&mut draft.afk_timeout_secs, 1..=30).suffix("s")
                .clamp_to_range(true));
            ui.end_row();
        });
        ui.add_space(3.0);
        ui.label(egui::RichText::new(
            "When AFK detection is enabled (Bridge tab), VTS is told the face is\n\
             lost after this many seconds of no tracking data."
        ).small().color(egui::Color32::from_gray(130)));

        ui.add_space(10.0);
        ui.label(egui::RichText::new("Appearance").strong());
        ui.separator();
        egui::Grid::new("appear_grid").num_columns(2).spacing([12.0, 6.0]).min_col_width(120.0).show(ui, |ui| {
            ui.label("Theme");
            ui.horizontal(|ui| {
                ui.selectable_value(&mut draft.theme, "dark".into(),  "Dark");
                ui.selectable_value(&mut draft.theme, "light".into(), "Light");
            });
            ui.end_row();
        });

        ui.add_space(10.0);
        ui.label(egui::RichText::new("Logging").strong());
        ui.separator();
        egui::Grid::new("log_grid").num_columns(2).spacing([12.0, 6.0]).min_col_width(120.0).show(ui, |ui| {
            ui.label("Log level");
            ui.horizontal(|ui| {
                for lvl in ["error", "warn", "info", "debug"] {
                    ui.selectable_value(&mut draft.log_level, lvl.to_string(), lvl.to_ascii_uppercase());
                }
            });
            ui.end_row();
        });

        ui.add_space(16.0);
        ui.separator();
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.add_enabled(*draft != *settings, egui::Button::new("Save Settings")).clicked() {
                *settings = draft.clone();
                settings.save();
                *status = "Saved".into();
            }
            if ui.button("Reset Defaults").clicked() {
                *draft = AppSettings::default();
                *status = "Reset to defaults (not saved)".into();
            }
        });
        if !status.is_empty() {
            ui.add_space(4.0);
            let col = if status.starts_with("Saved") {
                egui::Color32::from_rgb(80, 200, 100)
            } else {
                egui::Color32::from_gray(150)
            };
            ui.label(egui::RichText::new(status.as_str()).small().color(col));
        }

        ui.add_space(12.0);
        ui.separator();
        ui.add_space(6.0);
        ui.label(egui::RichText::new("Config files").strong());
        ui.add_space(3.0);
        let dir = app_dir().to_string_lossy().into_owned();
        egui::Grid::new("paths_grid").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
            for (label, file) in [
                ("App settings", "settings.json"),
                ("UI config",    "ui-cfg.json"),
                ("Auth token",   "token"),
                ("Logs",         "log/log.log"),
            ] {
                ui.label(egui::RichText::new(label).small().color(egui::Color32::from_gray(140)));
                ui.label(egui::RichText::new(format!("{dir}/{file}")).small().monospace().color(egui::Color32::from_gray(110)));
                ui.end_row();
            }
        });
    });
}

// ── About tab ─────────────────────────────────────────────────────────────────

fn about_ui(
    ui: &mut egui::Ui,
    update_rx:   &mut Option<Receiver<Option<UpdateInfo>>>,
    update_info: &Option<UpdateInfo>,
    update_open: &mut bool,
) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.add_space(16.0);
        ui.vertical_centered(|ui| {
            ui.label(egui::RichText::new(APP_NAME).size(24.0).strong()
                .color(egui::Color32::from_rgb(100, 170, 255)));
            ui.add_space(4.0);
            ui.label(egui::RichText::new(format!("v{VERSION}")).size(13.0)
                .color(egui::Color32::from_gray(160)));
            ui.add_space(8.0);
            ui.label(egui::RichText::new(
                "Cross-platform bridge between face tracking\nsources and VTube Studio."
            ).color(egui::Color32::from_gray(200)));
            ui.add_space(4.0);
            ui.label(egui::RichText::new("Free & open-source alternative to VBridger.")
                .small().color(egui::Color32::from_gray(140)));
        });

        ui.add_space(16.0);
        ui.separator();
        ui.add_space(10.0);

        ui.label(egui::RichText::new("Fork & Authors").strong());
        ui.add_space(4.0);
        egui::Grid::new("authors_grid").num_columns(2).spacing([12.0, 5.0]).show(ui, |ui| {
            for (label, val) in [
                ("Original project", "rusty-bridge by ovROG"),
                ("This fork",        "rbridger by LakoMoor"),
                ("Repository",       "github.com/LakoMoor/RBridger"),
                ("Upstream",         "github.com/ovROG/rusty-bridge"),
            ] {
                ui.label(egui::RichText::new(label).color(egui::Color32::from_gray(150)));
                ui.label(egui::RichText::new(val).monospace().small().color(egui::Color32::from_gray(210)));
                ui.end_row();
            }
        });

        ui.add_space(10.0);
        ui.separator();
        ui.add_space(10.0);

        ui.label(egui::RichText::new("License").strong());
        ui.add_space(4.0);
        ui.label(egui::RichText::new("GNU General Public License v3.0")
            .color(egui::Color32::from_gray(210)));
        ui.add_space(2.0);
        ui.label(egui::RichText::new(
            "This program is free software: you can redistribute it and/or\n\
             modify it under the terms of the GNU General Public License as\n\
             published by the Free Software Foundation, either version 3, or\n\
             (at your option) any later version. Distributed WITHOUT WARRANTY."
        ).small().color(egui::Color32::from_gray(130)));

        ui.add_space(10.0);
        ui.separator();
        ui.add_space(10.0);

        ui.label(egui::RichText::new("Built with").strong());
        ui.add_space(4.0);
        egui::Grid::new("tech_grid").num_columns(2).spacing([12.0, 4.0]).show(ui, |ui| {
            for (name, desc) in [
                ("Rust",          "Systems programming language"),
                ("egui / eframe", "Immediate-mode GUI framework"),
                ("tungstenite",   "WebSocket client for VTS API"),
                ("evalexpr",      "Transform formula evaluation"),
                ("ort / ONNX",    "Built-in webcam inference runtime"),
                ("nokhwa",        "Cross-platform camera capture"),
            ] {
                ui.label(egui::RichText::new(name).monospace().small().color(egui::Color32::from_gray(200)));
                ui.label(egui::RichText::new(desc).small().color(egui::Color32::from_gray(140)));
                ui.end_row();
            }
        });

        ui.add_space(12.0);
        ui.separator();
        ui.add_space(8.0);

        ui.vertical_centered(|ui| {
            let checking = update_rx.is_some();
            if checking {
                ui.spinner();
                ui.label(egui::RichText::new("Checking for updates…")
                    .small().color(egui::Color32::from_gray(150)));
            } else {
                match update_info {
                    Some(info) => {
                        ui.label(egui::RichText::new(format!("Version {} is available!", info.version))
                            .color(egui::Color32::from_rgb(80, 200, 100)));
                        if ui.small_button("Open release page").clicked() {
                            *update_open = true;
                        }
                    }
                    None => {
                        if ui.button("Check for updates").clicked() {
                            let (tx, rx) = mpsc::channel();
                            thread::spawn(move || { let _ = tx.send(check_for_update()); });
                            *update_rx = Some(rx);
                        }
                    }
                }
            }
        });

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(8.0);
        ui.vertical_centered(|ui| {
            ui.label(egui::RichText::new("Copyright © 2024-2025  LakoMoor / ovROG")
                .small().color(egui::Color32::from_gray(95)));
        });
        ui.add_space(8.0);
    });
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() {
    let data_dir = app_dir();
    let _ = std::env::set_current_dir(&data_dir);

    let log_dir  = data_dir.join("log");
    let _ = fs::create_dir_all(&log_dir);
    let log_file = log_dir.join("log.log").to_string_lossy().into_owned();
    let log_cfg  = include_str!("../../configs/log_cfg.yml").replace("log/log.log", &log_file);
    if let Ok(raw) = serde_yaml::from_str(&log_cfg) {
        let _ = log4rs::init_raw_config(raw);
    }

    // Restore saved window size
    let init_cfg = Config::load();
    let init_w = init_cfg.window_w.unwrap_or(460.0).max(380.0);
    let init_h = init_cfg.window_h.unwrap_or(440.0).max(340.0);

    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../resources/rb128.png")).ok();
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(APP_NAME)
        .with_inner_size([init_w, init_h])
        .with_min_inner_size([380.0, 340.0])
        .with_resizable(true);
    if let Some(icon) = icon {
        viewport = viewport.with_icon(std::sync::Arc::new(icon));
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(APP_NAME, options, Box::new(|cc| Ok(Box::new(App::new(cc))))).unwrap();
}
